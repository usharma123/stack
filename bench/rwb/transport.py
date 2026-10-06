"""Command transports. Each run owns its containers/projects and may remove only those.

DockerTransport: one disposable container per tool per run, named rwb-<tool>-<run id> and
labelled with the run id. Timings: outer_ns is host perf_counter_ns around `docker exec`
(includes Docker transport); inner_ns is measured inside the container by bash builtins
($EPOCHREALTIME, no extra process) around the step body only.

HostTransport: commands launched directly on the host (used by Compose, whose engine is
the host Docker daemon). Only outer_ns exists.
"""
import os
from pathlib import Path
import re
import shlex
import signal
import subprocess
import time

from .record import utc

OWNER_LABEL = "rwb.owner=stack-realworld-bench"
NAME = re.compile(r"^rwb-[a-z0-9-]+$")

# Body runs in a subshell so `exit` inside it is captured. $$ is the session id because
# the wrapper runs under `setsid -w`; the harness kills that group on timeout.
WRAPPER = r'''
mkdir -p /tmp/rwb-pids && echo $$ > "/tmp/rwb-pids/$2"
__rwb_s=${EPOCHREALTIME/./}
( eval "$1" )
__rwb_rc=$?
__rwb_e=${EPOCHREALTIME/./}
if [ -n "$__rwb_s" ]; then
  printf '\n@@RWB-INNER %s000 %s000 %s\n' "$__rwb_s" "$__rwb_e" "$__rwb_rc" >&2
fi
exit $__rwb_rc
'''


_q = shlex.quote


class OwnershipError(RuntimeError):
    pass


def _communicate(proc, timeout):
    """Run to completion or kill its process group on timeout, bounding output drain."""
    try:
        out, err = proc.communicate(timeout=timeout)
        return proc.returncode, out, err, False
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            out, err = proc.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            out, err = b"", b"output drain timed out\n"
        return 124, out or b"", err or b"", True


class Runner:
    """Thin seam so tests can substitute subprocess execution."""

    def __call__(self, argv, timeout, env=None, cwd=None):
        start = time.perf_counter_ns()
        proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, start_new_session=True, env=env, cwd=cwd)
        code, out, err, timed_out = _communicate(proc, timeout)
        return code, out, err, timed_out, time.perf_counter_ns() - start


class DockerTransport:
    kind = "docker-exec"

    def __init__(self, recorder, run_id, tool, image, mounts, user="agent", runner=None,
                 docker="docker", resources=()):
        self.recorder, self.image, self.user = recorder, image, user
        self.name = f"rwb-{tool}-{run_id}"
        if not NAME.match(self.name):
            raise ValueError(f"bad container name {self.name}")
        self.run_id, self.mounts, self.docker = run_id, mounts, docker
        self.resources = list(resources)
        self.run = runner or Runner()
        self.created = False
        self.image_id = None

    def _docker(self, label, phase, args, timeout=120):
        seq = self.recorder.next_seq()
        argv = [self.docker, *args]
        started = utc()
        code, out, err, timed_out, ns = self.run(argv, timeout)
        return self.recorder.write(seq, label, phase, argv, "host", code, ns, out, err, timed_out, started)

    def start(self):
        result = self._docker("image-inspect", "meta", ["image", "inspect", "--format", "{{.Id}}", self.image])
        if not result.ok:
            raise RuntimeError(f"image {self.image} unavailable")
        self.image_id = result.stdout.strip()
        args = ["run", "-d", "--init", "--name", self.name, "--label", OWNER_LABEL,
                "--label", f"rwb.run={self.run_id}", *self.resources]
        for host, container in self.mounts:
            args += ["-v", f"{Path(host).resolve()}:{container}:ro"]
        # ev-nix based images exec "$@" from their entrypoint; others run it directly.
        args += [self.image, "sleep", "infinity"]
        result = self._docker("container-create", "meta", args)
        if not result.ok:
            raise RuntimeError(f"could not create {self.name}: {result.stderr.strip()}")
        self.created = True
        return result

    def exec(self, label, phase, body, timeout=600, user=None):
        if not self.created:
            raise RuntimeError("container not started")
        seq = self.recorder.next_seq()
        argv = [self.docker, "exec", "-u", user or self.user, "-w", "/tmp", self.name,
                "setsid", "-w", "bash", "-c", WRAPPER, "rwb", body, str(seq)]
        started = utc()
        code, out, err, timed_out, ns = self.run(argv, timeout)
        if timed_out:
            # Kill only this step's process group inside our own container.
            self.run([self.docker, "exec", "-u", "root", self.name, "bash", "-c",
                      f'kill -KILL -- -"$(cat /tmp/rwb-pids/{seq})"'], 30)
        return self.recorder.write(seq, label, phase, argv, self.kind, code, ns, out, err, timed_out,
                                   started, dict(body=body))

    def copy_out(self, path, dest):
        """Copy a path out of our own container (artifact collection); None if absent."""
        probe = self.exec("artifact-probe", "cleanup", f"test -e {_q(path)}", timeout=30)
        if not probe.ok:
            return None
        Path(dest).parent.mkdir(parents=True, exist_ok=True)
        return self._docker("artifact-copy", "cleanup", ["cp", f"{self.name}:{path}", str(dest)], timeout=300)

    def verify_owned(self):
        """Refuse to touch a container unless it carries this run's labels."""
        code, out, _, _, _ = self.run([self.docker, "inspect", "--format",
                                       '{{index .Config.Labels "rwb.owner"}} {{index .Config.Labels "rwb.run"}}',
                                       self.name], 30)
        if code != 0:
            return False
        if out.decode().split() != [OWNER_LABEL.split("=", 1)[1], self.run_id]:
            raise OwnershipError(f"{self.name} exists but is not owned by run {self.run_id}")
        return True

    def destroy(self):
        if not self.created:
            return None
        if not self.verify_owned():
            return None
        result = self._docker("container-remove", "cleanup", ["rm", "-f", self.name])
        if result.ok:
            self.created = False
        return result

    def gone(self):
        code, out, _, _, _ = self.run([self.docker, "ps", "-a", "--filter", f"label=rwb.run={self.run_id}",
                                       "--format", "{{.Names}}"], 30)
        return code == 0 and self.name not in out.decode().split()


class HostTransport:
    """Runs the same wrapped bash bodies directly on the host, in a run-owned directory."""
    kind = "host"

    def __init__(self, recorder, workdir, env=None, runner=None, bash="bash"):
        self.recorder, self.workdir, self.env, self.bash = recorder, Path(workdir), env, bash
        self.run = runner or Runner()

    def copy_out(self, path, dest):
        """Copy a host path (inside the run-owned workdir) into the results; None if absent."""
        if not Path(path).exists():
            return None
        Path(dest).parent.mkdir(parents=True, exist_ok=True)
        return self.exec("artifact-copy", "cleanup", f"cp -R {_q(path)} {_q(str(dest))}", timeout=300)

    def exec(self, label, phase, body, timeout=600, user=None):
        seq = self.recorder.next_seq()
        argv = [self.bash, "-c", WRAPPER.replace('/tmp/rwb-pids', str(self.workdir / "pids")),
                "rwb", body, str(seq)]
        started = utc()
        code, out, err, timed_out, ns = self.run(argv, timeout, env=self.env, cwd=str(self.workdir))
        return self.recorder.write(seq, label, phase, argv, self.kind, code, ns, out, err,
                                   timed_out, started, dict(body=body))
