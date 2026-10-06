"""Check Lando's merged runtime config without echoing it.

Usage:
  lando config --format json --path setup | python3 -I check_config.py setup
  lando config --format json --path orchestratorBin | python3 -I check_config.py orchestrator EXPECTED

`lando config --path P` returns the WHOLE config (including process.env) when P is unset
(lib/formatters.js: `_.get(data, path, data)`), so stdout is parsed here and only the
checked values are printed. Source: lando/core 7a87f80576c5cdb5c7d616108bc9aff81150d463.

setup: autosetup (hooks/lando-run-setup.js, run by start/stop/destroy) must not install the
host CA (sudo), Docker Desktop, buildx into $HOME/.docker/cli-plugins, plugins, or another
orchestrator. orchestrator: the configured orchestratorBin survived utils/build-config.js
(absolute, existing) and is the expected checksum-verified private executable.
"""
import json
import os
import sys

SETUP = {"skipInstallCa": True, "buildEngine": False, "buildx": False, "orchestrator": False,
         "installPlugins": False, "skipCommonPlugins": True}


def parse(text):
    for line in reversed(text.splitlines()):
        line = line.strip()
        if line:
            try:
                return json.loads(line)
            except ValueError:
                continue
    raise SystemExit("lando config printed no JSON")


def check_setup(value):
    if not isinstance(value, dict) or "userConfRoot" in value:
        raise SystemExit("lando config has no `setup` object")
    got = {k: value.get(k) for k in SETUP}
    print("lando setup " + " ".join(f"{k}={json.dumps(v)}" for k, v in got.items()))
    wrong = [k for k, v in SETUP.items() if got[k] is not v]
    if wrong:
        raise SystemExit("lando autosetup would act on the host: " + ", ".join(wrong))


def check_orchestrator(value, expected):
    if not isinstance(value, str):
        raise SystemExit("lando orchestratorBin is unset: Lando would select or download another orchestrator")
    print(f"lando orchestratorBin {value}")
    if os.path.realpath(value) != os.path.realpath(expected) or not os.access(value, os.X_OK):
        raise SystemExit(f"lando orchestratorBin is not the verified private executable {expected}")


if __name__ == "__main__":
    mode, rest = sys.argv[1], sys.argv[2:]
    data = parse(sys.stdin.read())
    if mode == "setup":
        check_setup(data)
    elif mode == "orchestrator" and len(rest) == 1:
        check_orchestrator(data, rest[0])
    else:
        raise SystemExit("usage: check_config.py setup | orchestrator EXPECTED")
