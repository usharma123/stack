#!/usr/bin/env python3
"""Render a source-linked report from the October 5 release comparison receipts."""
import argparse
import html
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('results',type=Path)
parser.add_argument('--report',type=Path)
parser.add_argument('--overwrite',action='store_true',help='explicitly regenerate derived report and summary')
args=parser.parse_args()
base=args.results.resolve()
dest=(args.report or base.parents[1]/'stack-0.1.4-report.html').resolve()
if not args.overwrite and (dest.exists() or (base/'summary.json').exists()):
    parser.error('derived output exists; use --overwrite to explicitly regenerate it')
# The prose below describes a completed comparison, so partial receipts cannot
# support it. Ignore Python environment options so verifier assertions stay on.
verification=subprocess.run(
    [sys.executable,'-I',str(Path(__file__).with_name('verify-comparison.py')),str(base)],
    capture_output=True,text=True,
)
if verification.returncode:
    parser.error('receipt validation failed; no derived output written:\n'+verification.stderr.strip())
e=html.escape
def read(path,default=None):
    p=base/path
    return json.loads(p.read_text()) if p.exists() else default
def table(headers,rows):
    return '<div class="table"><table><thead><tr>'+''.join('<th>'+e(str(v))+'</th>' for v in headers)+'</tr></thead><tbody>'+''.join('<tr>'+''.join('<td>'+str(v)+'</td>' for v in row)+'</tr>' for row in rows)+'</tbody></table></div>'
def link(path,text):
    return '<a href="'+e(os.path.relpath(base/path,dest.parent),quote=True)+'">'+e(text)+'</a>'
def source(url,label): return '<a href="'+e(url,quote=True)+'">'+e(label)+'</a>'
names={'stack':'Stack 0.1.4','mise':'mise 2026.10.3','flox':'Flox 1.17.0','devbox':'Devbox 0.18.4','devenv-latest':'devenv 2.4.0','nix':'Determinate Nix 3.23.0 / Nix 2.35.2','compose':'Compose 5.6.0','pixi':'Pixi 0.81.0'}
timing=[]
measurements={}
for tool,name in names.items():
    steps=read(tool+'/steps.json',[])
    samples=[r['seconds']*1000 for r in steps if r['step'].startswith('exec-') and 'required' not in r['step'] and r['code']==0]
    if samples:
        median=statistics.median(samples)
        measurements[tool]={'n':len(samples),'median_ms':median,'min_ms':min(samples),'max_ms':max(samples)}
        timing.append([e(name),f'{median:.1f} ms',f'{min(samples):.1f}–{max(samples):.1f} ms',len(samples),link(tool+'/steps.json','commands + timings')])
    else:
        timing.append([e(name),'Not measured','—',0,link(tool+'/steps.json','attempt receipts')])

setups=[]
for tool in ['stack','mise','flox','devbox','devenv-latest','nix']:
    steps={x['step']:x for x in read(tool+'/steps.json',[])}
    a,b=steps.get('setup-first'),steps.get('setup-second')
    if not a: continue
    semantics='Resolve/install and start services' if tool in ['stack','mise'] else 'Prepare tool environment; service startup separate'
    setups.append([e(names[tool]),f'{a["seconds"]:.2f}s / exit {a["code"]}',f'{b["seconds"]:.2f}s / exit {b["code"]}' if b else 'Not reached',semantics])

scenario_rows=[]
for item in read('release-e2e/scenarios.json',[]):
    scenario_rows.append([e(item['scenario']), 'PASS' if item['code']==0 else 'FAIL',f'{item["seconds"]:.2f}s',link('release-e2e/'+item['scenario']+'.log','assertion log')])
native=[]
native_path=base/'native-scenarios.jsonl'
if native_path.exists():
    for line in native_path.read_text().splitlines():
        x=json.loads(line)
        native.append([e(x['scenario']),e(x['result']),str(x.get('seconds','—'))+'s'])
pilot=read('pilot/summary.json')
pilot_html=table(['Phase','Tasks passed','Wrong instances','Orphaned services','Injected failures handled','Verified exec p50'],[
    [e(name),str(p['tasks_succeeded'])+'/'+str(p['tasks_attempted']),p['wrong_instance_incidents'],p['orphaned_service_processes'],str(p['controlled_failures']['handled'])+'/'+str(p['controlled_failures']['injected']),str(p['latency_ms']['exec_verified']['p50'])+' ms']
    for name,p in pilot['phases'].items()
])+ '<p>'+link('pilot/summary.json','Full pilot summary')+' · '+link('pilot-progress.log','Pilot output')+'</p>' if pilot else '<p>No completed pilot summary yet.</p>'

sources=[
('https://nixos.org/guides/how-nix-works/','Nix package and dependency model'),
('https://nix.dev/manual/nix/stable/command-ref/conf-file.html','Nix configuration, including build sandbox settings'),
('https://flox.dev/docs/concepts/environments','Flox environments and lockfiles'),
('https://flox.dev/docs/concepts/services/','Flox service lifetime and shutdown'),
('https://flox.dev/docs/concepts/composition','Flox manifest composition and overrides'),
('https://flox.dev/docs/concepts/nix-expression-builds','Flox Nix expression builds'),
('https://devenv.sh/processes/','devenv port allocation, probes, supervision, process dependencies'),
('https://devenv.sh/mcp/','devenv MCP server'),
('https://devenv.sh/','devenv services, tasks, profiles and integrations'),
('https://www.jetify.com/docs/devbox/guides/services/','Devbox service management'),
('https://mise.jdx.dev/daemons.html','mise daemons'),
('https://mise.jdx.dev/dev-tools/mise-lock.html','mise lockfile and backend-dependent artifact metadata'),
('https://docs.docker.com/compose/how-tos/networking/','Compose project networks and service discovery'),
('https://docs.docker.com/compose/how-tos/startup-order/','Compose health-based startup ordering'),
('https://pixi.prefix.dev/latest/workspace/lock_file/','Pixi lockfile'),
('https://pixi.prefix.dev/latest/workspace/multi_platform_configuration/','Pixi platform configuration'),
('https://containers.dev/features','Dev Container reusable features'),
('https://direnv.net/','direnv scope'),
('https://asdf-vm.com/','asdf scope'),
('https://docs.astral.sh/uv/concepts/projects/sync/','uv dependency locking'),
('https://devpod.sh/docs/what-is-devpod','DevPod workspace provisioning'),
('https://coder.com/docs/user-guides/devcontainers','Coder Dev Containers integration'),
('https://developer.hashicorp.com/vagrant/docs','Vagrant VM lifecycle'),
('https://spack.readthedocs.io/en/latest/environments.html','Spack environments and concretized dependency graphs'),
]

verdict=[
['Agent commands using multiple local Postgres/Redis checkouts','Stack has a demonstrated advantage with the tested recipes','Automatic independent ports, checks of the server reached through application connection settings, and withheld endpoints after shutdown.'],
['Lowest repeated CLI-entry overhead','Stack loses to mise and Flox in this run','Stack performs live service checks. The observed extra work costs time.'],
['Complete package/build dependency reproducibility','Nix family is stronger by design','Stack locks bundle content and exact release names. It does not lock the entire native dependency graph or downloaded tool artifacts.'],
['Runtime filesystem/process/network isolation','Containers or VMs provide capabilities Stack lacks','Stack executes trusted bundles and commands on the host. It is not a security boundary.'],
['Broader preconfigured services and development integrations','devenv has broader documented coverage','Stack supplies five presets and arbitrary custom commands. It is a smaller system with a different emphasis.'],
['Native Windows and scientific environment solving','Pixi has the broader documented fit','Stack publishes macOS and Linux binaries. Pixi resolves platform-specific Conda/PyPI environments, including Windows.'],
['Cross-project service isolation with deliberate configuration','Several tools can pass','Explicit-port mise/Flox/Devbox, correctly wired devenv allocation and isolated Compose networks all passed the tested isolation checks.'],
['Remote workspace provisioning, IDE access and organization management','DevPod/Coder address a different requirement','Stack does not provision VMs, remote IDE workspaces, or a workspace control plane.'],
['Every OS, service, scale and failure condition','No universal winner established','This report is a bounded local evaluation, not proof over all possible environments.'],
]

matrix=[
['Bundle content pinned across a moved Git tag','Tested separately with an empty Stack bundle cache','A content-pinned bundle is different from a fully pinned tool/build closure.'],
['Exact runtime/tool release locking','Linux scenario 1 checks pins and running Postgres version','Checksums of downloaded provider artifacts remain outside stack.lock.'],
['Two independent project directories','Fresh measured pass for Stack','mise/Flox/devenv also pass with correct per-project port configuration.'],
['Wrong database reached through app URLs','Fresh observed failures in default comparison recipes; Stack checks identity','This is an accidental-misrouting defense, not protection against hostile code ignoring the environment.'],
['Independent service shutdown','Fresh pass for Stack and corrected comparison configurations','A directory with a new name is not by itself proof that its application uses a new database.'],
['Repeated up','Measured in baseline and service suite','Successful command exit alone does not prove service readiness; inspect the subsequent probe.'],
['TTL renewal and expiry','Stack scenario 3','Unattended expiry requires a running gc watcher or other invocations; Stack installs no supervisor for GC.'],
['Commands outliving a short TTL','Stack scenario 3','Active execution leases protect the running command in the tested case.'],
['Owner process death','Stack scenario 3 and scripted pilot','Do not infer universal cleanup after every crash or host failure.'],
['MCP command timeout and descendant cleanup','Stack scenario 4','Unix process-group behavior; no Windows claim.'],
['OCI bundle publish, consume and tag replacement refusal','Stack scenario 5, local registry','No private cloud registry, credential helper, cross-region or enterprise auth performance test.'],
['Changed configuration invalidates existing session','Stack scenario 6','A successful generation check is not arbitrary application health monitoring.'],
['Deleted or replaced checkout','Stack scenario 7 deliberately refuses unsafe cleanup','Safe refusal leaves live/uncertain services to explicit cleanup. That is an operational limitation, not automatic recovery success.'],
['Custom service identity','Stack scenario 8 tests an opted-in identity probe','Without a probe, custom services and non-Postgres/Redis presets are liveness-only.'],
['Concurrent work and injected failures','Separate Stack scripted pilot','No matched competitor productivity study; scripted workers are not autonomous coding agents.'],
['Linux ARM64 runtime','Measured in Docker on this Mac','Container kernel/runtime and image caches are part of the environment.'],
['macOS ARM64 runtime','Native Stack suite reported below','No equivalent native competitor comparison in this run.'],
['Linux/macOS x64 and Windows','Not run','The existence of an x64 release binary is not an x64 service benchmark.'],
['Offline setup, proxy/firewall failures, lost registries','Not established by this run','Do not claim air-gapped reproducibility from ordinary cache reuse.'],
['Disk full, power loss, suspend/resume, kernel kill, corrupt disk','Not run','Crash consistency and recovery beyond injected scenarios remain open.'],
['Many users, hostile tenants, hundreds of services','Not run / outside Stack isolation model','Ports on a shared host and process ownership are not tenant security isolation.'],
['CPU, RSS, total disk and battery cost','Not measured as comparable steady-state workloads','The Stack binary size is not the total installed environment footprint.'],
['Real coding-agent success, token spend, completion time','Not measured','No claim of agent productivity superiority is justified.'],
]

adjacent=[
['Dev Containers','Reusable container features and editor integration','Stronger fit when the developer environment itself must be containerized. Not separately timed.',sources[16][0]],
['DevPod','Local or remote devcontainer workspaces','Provisioning and connecting a workspace is outside Stack’s current scope. Not installed.',sources[20][0]],
['Coder','Managed remote workspaces and devcontainer access','An organization/workspace layer, not an interchangeable local service CLI. Not deployed.',sources[21][0]],
['Vagrant','VM lifecycle and provisioning','Use when a VM/kernel boundary is required. No VM benchmark run.',sources[22][0]],
['Spack','Scientific/HPC package variants and dependency concretization','Broader compiler/build-variant work than Stack’s service bundle layer. Not installed.',sources[23][0]],
['asdf','Per-project runtime version management','A tool-version manager rather than a service/session layer. Not timed.',sources[18][0]],
['direnv','Automatic directory-based environment activation','Complementary to several compared tools. No native service identity or lease comparison.',sources[17][0]],
['uv','Python dependency locking and environment management','Already used by the fixture; complements Stack. Does not replace the database/session layer.',sources[19][0]],
]

report='''<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Stack 0.1.4 competitor benchmark</title><style>
:root{font-family:system-ui,-apple-system,sans-serif;color:#17212b;background:#f3f5f7;line-height:1.6}body{margin:0}main{max-width:1180px;margin:0 auto;padding:46px 28px 80px;background:white}h1{font-size:2.5rem;line-height:1.15;max-width:900px}h2{font-size:1.55rem;margin-top:42px;border-top:1px solid #dce2e7;padding-top:24px}h3{margin-top:25px}p,li{max-width:1000px}a{color:#005bb5}code,pre{font-family:ui-monospace,monospace;font-size:.88em}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#f3f5f7;padding:18px;border-radius:6px}.table{overflow:auto}table{border-collapse:collapse;width:100%;font-size:.91rem;margin:18px 0}th,td{padding:12px;text-align:left;vertical-align:top;border-bottom:1px solid #dce2e7}th{background:#edf2f6}td:first-child{font-weight:600;min-width:150px}.lead{font-size:1.2rem}.note{padding:16px 20px;border-left:4px solid #59748c;background:#f0f4f7}.meta{color:#536575}.verdict{padding:20px 24px;background:#ecf4ee;border-left:5px solid #337343}nav{display:flex;flex-wrap:wrap;gap:15px;font-size:.95rem}@media(max-width:650px){main{padding:25px 16px}h1{font-size:2rem}td,th{padding:9px}}
</style></head><body><main>
<p class="meta">Measured October 5, 2026 · Published release · Apple M4 Pro / Linux ARM64 containers</p>
<h1>Stack 0.1.4 does not win in every condition.</h1>
<p class="lead">Its strongest demonstrated advantage is preventing accidental cross-checkout database use in local agent workflows. That advantage comes with command-entry overhead and does not replace Nix’s package model, container isolation, or a remote workspace platform.</p>
<nav><a href="#verdict">Verdicts</a><a href="#method">Method</a><a href="#runtime">Measured results</a><a href="#correctness">Isolation</a><a href="#stack">Stack verification</a><a href="#coverage">Condition coverage</a><a href="#competitors">Competitors</a><a href="#next">Priorities</a><a href="#sources">Sources</a></nav>
<h2 id="verdict">Where Stack wins, loses, or has not proved a win</h2>
'''+table(['Condition','Verdict','Reason'],verdict)+'''
<h2 id="method">What was actually compared</h2>
<p>The tested artifact is <code>@ushawarma/stack@0.1.4</code>, downloaded with <code>npm pack</code>. Its build metadata identifies commit <code>2e4eea4293a4001ce98a5fd374b83a1476b8ec52</code>, which matched this checkout and remote main when the run began. Linux and macOS binaries were checked against the package’s recorded SHA-256 values. This verifies artifact identity against its included metadata; it is not an independent publisher attestation audit.</p>
<p>The host has an Apple M4 Pro and 24 GiB RAM. Docker reports ARM64, 14 CPUs and approximately 15.6 GiB available to its Linux VM. Each primary tool run used its own new container and non-root <code>agent</code> account. Existing background workloads were left running. There was no dedicated CPU isolation or randomized run order.</p>
<p>The common fixture uses Python 3.13, uv, Postgres 17 and Redis, with three application tests that exercise Python and both services. Resolved patch versions differ between ecosystems. Stack additionally includes its example jq tool and bundle files. This is a realistic workflow comparison, not a controlled benchmark of identical binary closures.</p>
<p>Stack and mise use mise 2026.10.3 with Pitchfork 2.29.0. Flox 1.17.0 and Devbox 0.18.4 matched upstream release listings. The base devenv image had 2.3.1; an ordinary profile upgrade left that version unchanged, so 2.4.0 was explicitly installed from its release flake for the current comparison. The tested Nix is the Determinate distribution, reporting Nix 2.35.2; it must not be relabeled an upstream stable Nix build. Compose 5.6.0 was downloaded separately from the older Docker Desktop plugin. Pixi 0.81.0 was downloaded separately.</p>
<p>Plain Nix was tested as a toolchain shell. It does not have a built-in equivalent of <code>stack up</code>; service lifecycle is therefore unscored rather than failed. Compose was tested as two service projects, with database/cache markers and private networks, not the Python application fixture. Pixi’s service launch/stop commands were authored benchmark glue, with explicit ports, not a native Pixi supervisor.</p>
<p class="note">The image caches were not equalized. “First setup” means a new project in a new container with the tool image already present, not an empty machine, empty Nix store, cold internet/CDN, or tool installation from scratch. All timings are single observations except repeated command entry. Existing October 2 numbers are not reused in this report.</p>
<p>Harness issues were preserved rather than counted as product failures. Standalone Compose initially encountered the host credential-helper configuration; Pixi’s base container exited because the runner omitted a keepalive; the CLI contract probe initially expected the wrong error-code name; and Devbox’s readiness loop was mangled by nested command-string quoting. Corrected runs used isolated public-image Docker configuration, a keepalive, Stack’s actual <code>usage</code> code, and an external readiness script. The original Devbox readiness timing is invalid and excluded from readiness conclusions; its application tests and database identity query remain valid. The final configured Devbox run passed the corrected probe. The current devenv allocated-port follow-up overlapped Linux E2E checks; its timing is not in the timing table.</p>
<p>Evidence: '''+link('package/package/build-info.json','published build metadata')+' · '+link('upstream-releases.json','upstream release receipts')+' · '+link('summary.json','machine-readable timing summary')+'''. Exact commands and return codes are recorded in each tool’s <code>steps.json</code>, with separate stdout and stderr files.</p>
<h2 id="runtime">Repeated command entry</h2>
<p>Fifteen invocations per completed row, reporting wall-clock median and observed range. Each invocation starts a fresh CLI process. Linux tool rows include <code>docker exec</code> transport and a shell; Compose has its own client transport. Pixi uses an explicitly configured environment. These are user-visible command paths, not isolated measurements of parser or syscall overhead.</p>
'''+table(['Tool','Median','Observed min–max','n','Evidence'],timing)+'''
<p>Stack’s command path verifies configured live services. Mise and the other shell-entry paths do not perform an equivalent identity check. Comparing their timing is useful for the latency the user pays, but not proof of inferior implementation efficiency. Nix’s measured <code>nix develop</code> path reevaluates the shell; cached activation with nix-direnv is a different workflow and was not measured.</p>
<p>Stack also ran fifteen <code>exec --require-all -- true</code> samples. Their data are preserved in the same log. Adding <code>--require-all</code> changes refusal behavior; it does not remove the need to verify live services.</p>
<h3>Setup observations, not a ranked race</h3>
'''+table(['Tool','First setup','Second project setup','What this step includes'],setups)+'''
<p>Do not compare the first two rows directly with the others as “time to healthy services”: Stack/mise include service startup in setup, while Flox, Devbox and devenv use separate startup and readiness steps. A fast failed second setup is not a performance win. Per-tool logs retain startup, readiness and application-test timings separately.</p>
<h2 id="correctness">The important result: a passing test can use the wrong database</h2>
<p>The initial recipes intentionally used the repository’s existing straightforward service configurations. They are an accidental-collision test, not a claim that every recommended configuration of each tool fails. The check ran <code>SHOW data_directory</code> through each application’s own <code>DATABASE_URL</code>, then stopped project A and reran B’s application tests.</p>
'''+table(['Configuration','Observed result','Evidence'],[
['Stack 0.1.4','A and B had different ports and database directories. B’s three tests still passed after A stopped. Stopped A exported unverified.stack.invalid endpoints.',link('stack/identity-B.stdout','identity')+' · '+link('stack/B-after-A-stop.stdout','independent stop')],
['mise 2026.10.3, port="auto", separate plain directories','B’s daemon start correctly refused the occupied port, but direct mise exec still exported A’s endpoint. B’s tests passed against A, then failed after A stopped.',link('mise/setup-second.stderr','start refusal')+' · '+link('mise/identity-B.stdout','wrong identity')],
['Flox 1.17.0, fixed default ports','B’s services completed with exit 1 while B’s application URLs still reached A. B’s tests failed after A stopped. A held activation was used for background services.',link('flox/status-B.stdout','service failures')+' · '+link('flox/identity-B.stdout','wrong identity')],
['Devbox 0.18.4, default service plugin wiring','B’s application connected to /srv/appA/.devbox/virtenv/postgresql/data. Its tests failed after A stopped.',link('devbox/identity-B.stdout','wrong identity')+' · '+link('devbox/B-after-A-stop.stdout','dependency on A')],
['Devbox 0.18.4, explicit PGPORT/REDIS_PORT and matching URLs','PASS. B reached its own database and kept passing all three application tests after A stopped. The plugin’s supported port settings resolve the original collision.',link('devbox-configured/identity-B.stdout','correct identity')+' · '+link('devbox-configured/B-after-A-stop.stdout','independent stop')],
['devenv 2.4.0, URLs built from services.*.port','B reported Postgres 5433 and Redis 6380, but its application URLs remained 5432/6379 and reached A. This recipe used the configured base port rather than the allocated process port.',link('devenv-latest/status-B.stdout','allocated ports')+' · '+link('devenv-latest/identity-B.stdout','application endpoint')],
['mise and Flox, explicit per-project ports','Both reruns reached separate database directories and passed B’s application tests after A stopped. Stack is not the only capable tool.',link('mise-configured/B-after-A-stop.stdout','mise pass')+' · '+link('flox-configured/B-after-A-stop.stdout','Flox pass')],
['devenv 2.3.1, explicit service ports','Separate-directory and independent-stop tests passed. This remediation run used the older image CLI and is labeled accordingly.',link('devenv-configured/B-after-A-stop.stdout','explicit-port pass')],
['devenv 2.4.0, allocated-port URL recipe','PASS. Using config.processes.&lt;service&gt;.ports.main.value produced URLs on 5433/6380, reached B’s own database, and kept B working after A stopped. The initial mismatch is a configuration trap, not an inherent inability to propagate ports.',link('devenv-latest-dynamic/identity-B.stdout','correct allocated endpoint')+' · '+link('devenv-latest-dynamic/B-after-A-stop.stdout','independent stop')],
['Compose 5.6.0, project-private networks','PASS. Both PostgreSQL and Redis returned each project’s distinct marker. B’s marker remained available after A was removed. All test-owned containers were removed.',link('compose/steps.json','marker and cleanup assertions')],
['Pixi with authored launch/stop scripts','PASS. Explicit project-specific ports and data directories; both application fixtures passed, and B kept working after A stopped. This is configurable service operation, not a native Pixi identity or lease feature.',link('pixi/steps.json','scripted service run')],
])+'''
<p>Devenv’s documented allocated-port interface is '''+source('https://devenv.sh/processes/','processes.&lt;name&gt;.ports.&lt;name&gt;.value')+'''. The final verdict must account for the corrected recipe, not just retain an old fixture’s failure. Similarly, mise’s daemon-aware task path is different from direct <code>mise exec</code>, and worktree-specific port behavior is different from unrelated copied directories.</p>
<p>Repeated start commands returned zero for Stack, mise, Flox and devenv. Devbox’s second <code>services up -b</code> returned exit 1 with “process-compose is already running”; its running services remained usable. This is a retry-contract difference, not a service-isolation failure. '''+link('devbox/start-again.stderr','Devbox retry output')+'''</p>
<h2 id="stack">Verification of the published Stack release</h2>
<p>The Linux release suite runs the existing eight scenario scripts from the same commit as the downloaded artifact. It uses real mise, Pitchfork, PostgreSQL, Redis and a disposable local OCI registry. These are functional checks, not a comparative throughput benchmark or a complete security audit.</p>
'''+table(['Linux scenario','Result','Wall time','Evidence'],scenario_rows)+'''
<h3>Native macOS ARM64</h3>
'''+table(['Scenario','Result','Wall time'],native)+'''
<p>Native work uses isolated HOME, mise/Pitchfork state and Stack cache/registry directories. OCI coverage depends on the native runner’s recorded registry configuration. Missing/skipped entries must not be counted as passes. '''+link('native-progress.log','Full native output')+'''</p>
<h3>Scripted concurrency pilot</h3>
<p>This is a bounded replay of service operations, fixture tasks and controlled failures. It does not measure model intelligence, autonomous agent task success, or token costs. No competitor ran an equivalent pilot in this comparison, so it supports Stack’s tested reliability only.</p>
<p>Four concurrent projects, two rounds in each of two phases, sixteen tasks total. Each phase injected one service death and one owner/runner death. The fresh phase starts with empty isolated local caches but reuses installations in its second round; it is not eight independent cold machines. These native Mac timings are separate from the Docker command-entry table.</p>
'''+pilot_html+'''
<h3>Lockfile and machine-readable error checks</h3>
<p>A moved Git tag retained the original bundle commit and content hash when compiled with the saved lock into an empty bundle cache. An explicit update then changed the pin and reported the previous commit. Four failure probes emitted one JSON object and a nonzero exit: invalid subcommand (<code>usage</code>), impossible release and unknown tool (<code>resolve_failed</code>), and conflicting bundle values (<code>conflict</code>). '''+link('git-lock/summary.json','Git lock result')+' · '+link('contract/summary.json','CLI contract results')+'''</p>
<h2 id="coverage">Condition-by-condition evidence and limits</h2>
'''+table(['Condition','Evidence status','Limit'],matrix)+'''
<h2 id="competitors">Why each competitor still matters</h2>
<h3>Nix</h3>
<p>Nix’s package store identifies packages by their dependency graph and supports multiple versions, reusable binaries, atomic profile changes and rollbacks. Those are deeper package-management guarantees than Stack’s bundle and release-name lock. Stack is more directly tailored to starting independently verified local database sessions. A Nix development shell is not automatically a sandbox for arbitrary shell commands; build sandboxing is a separate mechanism. '''+source(sources[0][0],'Nix model')+' · '+source(sources[1][0],'Nix settings')+'''</p>
<h3>Flox</h3>
<p>Flox combines Nix-backed packages, a locked environment manifest, services and environment composition. Later manifests override earlier ones with warnings. It also has Nix expression builds for shipping packages. Stack’s bundle-local files, explicit conflict rejection and service identity enforcement fit a different local workflow. It would be wrong to call Flox unable to share internal tools merely because those tools require packaging. Background service lifetime follows active Flox activations; this test supplied a holder process. '''+source(sources[2][0],'Environment locks')+' · '+source(sources[3][0],'Services')+' · '+source(sources[4][0],'Composition')+' · '+source(sources[5][0],'Builds')+'''</p>
<h3>devenv</h3>
<p>Devenv supplies extensive language/service modules, task dependencies, readiness checks, restarts, profiles and development integrations. Its process manager supports automatic port allocation; application configuration must use the allocated value. It also has an MCP server, so “has MCP” is not a unique Stack advantage. The documented devenv MCP tools focus on package and option discovery, while Stack’s tested MCP exposes lifecycle operations and bounded execution. '''+source(sources[6][0],'Processes')+' · '+source(sources[7][0],'MCP')+' · '+source(sources[8][0],'Feature scope')+'''</p>
<h3>Devbox</h3>
<p>Devbox exposes Nix-backed packages through a JSON-oriented project setup and uses process-compose for services. It supports background services and custom process definitions. The initial fixture’s static URLs caused a real cross-project connection. A fresh rerun configured the plugins’ <code>PGPORT</code> and <code>REDIS_PORT</code> variables plus matching application URLs; isolation and independent shutdown then passed. The older report’s supervisor-crash finding was not rerun here and is not being restated as a current-release result. '''+source(sources[9][0],'Service management')+'''</p>
<h3>mise with Pitchfork</h3>
<p>Mise is Stack’s provider as well as its closest substitute. It already manages daemons, presets, readiness, tasks, automatic worktree ports and JSON state. Stack’s measured contribution is the additional composition, checkout allocation and verified-session contract. Mise’s supported backends can also record artifact URLs/checksums in mise.lock; Stack’s own exact release pins should not be described as stronger artifact locking. Features described by rolling mise docs may exceed the tested release; no untested new option is credited as a measured pass. '''+source(sources[10][0],'Daemons')+' · '+source(sources[11][0],'Lockfile semantics')+'''</p>
<h3>Docker Compose</h3>
<p>Compose provides project networks and container boundaries. Two projects can both call their database <code>postgres:5432</code> without host-port collisions when applications run in their own project networks. Health checks and <code>--wait</code> make startup observable. Host-port publishing, privileged containers, mounted host resources or deliberately shared networks change the isolation properties. Stack’s OCI bundle is a configuration artifact, not an OCI runtime image, so OCI support alone is not parity with Compose. '''+source(sources[12][0],'Networking')+' · '+source(sources[13][0],'Readiness')+'''</p>
<h3>Pixi</h3>
<p>Pixi resolves Conda/PyPI environments and stores package selections for configured platforms in a shared lockfile. That is useful for scientific/native dependency environments and Windows portability. It does not supply Stack’s checked service-session lifecycle. This run’s service scripts demonstrate that a toolchain manager can support the application when lifecycle and port choices are supplied explicitly. '''+source(sources[14][0],'Lockfile')+' · '+source(sources[15][0],'Platforms')+'''</p>
<h3>Adjacent alternatives</h3>
'''+table(['Tool','Main job','Comparison boundary','Primary source'],[[e(a),e(b),e(c),source(d,'docs')] for a,b,c,d in adjacent])+'''
<p>This is broad coverage of the main substitute categories, not a claim to have run every environment manager in existence. Guix, Conda/Mamba, Poetry, PDM, Nix-direnv, process-compose used directly, Overmind, Tilt, Skaffold, Codespaces and additional remote sandboxes are not independently benchmarked here. Choosing one would require its own matched workload and configuration.</p>
<h2 id="next">What would make Stack’s claim stronger</h2>
<ol><li><strong>Publish a narrow claim supported by the data.</strong> “Reusable local stacks with verified service identity and owned execution lifetimes” is defensible. “Better than Nix, Flox and containers in every condition” is not.</li>
<li><strong>Close the artifact-locking gap.</strong> Record or deliberately integrate platform-specific artifact hashes and backend identity. Test changed upstream artifacts and clean-machine reproduction, not only version strings.</li>
<li><strong>Reduce repeated verification cost without removing it.</strong> Profile where the extra command-entry time goes. A faster path must still refuse stale generations and wrong services; a cache that weakens those guarantees would trade away the reason to choose Stack.</li>
<li><strong>Improve lifecycle completion.</strong> Deleted-project GC currently prefers safe refusal. A provider operation that atomically checks ownership and stops the intended generation would enable more useful unattended cleanup.</li>
<li><strong>Expand service identity coverage.</strong> Presets beyond Postgres/Redis and arbitrary custom services need reliable identity adapters or probes before receiving the same safety claim.</li>
<li><strong>Run a matched reliability/productivity study.</strong> Equalize package versions and caches, use competitor-recommended configurations, randomize runs, test several machines and architectures, and compare actual agent completion and recovery effort. Include offline, interrupted downloads, long paths, disk pressure, suspend/resume and larger concurrency.</li></ol>
<p class="verdict">Recommendation: position Stack as a focused local orchestration layer for agents that must not silently reach another checkout’s database. Keep Nix/Pixi/containers as valid choices where their stronger dependency, platform or isolation capabilities are the requirement. No global win percentage is computed because the conditions have different importance and several remain untested.</p>
<h2 id="sources">Receipts and primary documentation</h2>
<p>Live documentation was retrieved on October 5, 2026. Documentation establishes a capability or intended contract, not that this run verified it. Release receipts and raw runtime outputs take precedence for tested-version claims. Some sites are rolling documentation, which is why exact version-specific conclusions above rely on command output.</p><ul>
'''+''.join('<li>'+source(url,label)+'</li>' for url,label in sources)+'''
</ul><h3>Reproduction and verification</h3>
<p>The new runners are under <code>eval/harness</code>. They expect the recorded <code>ev-*</code> images and the prepared package and platform binaries. Their scope is this ARM64 Mac and Docker ARM64 environment. Use a new result directory, because runners refuse to overwrite prior receipts. Keep the checkout at the release commit for matching E2E fixtures.</p>
<pre>mkdir -p eval/results/recheck
cp -R eval/results/latest-2026-10-05/package eval/results/latest-2026-10-05/bin eval/results/recheck/
python3 eval/harness/current-benchmark.py eval/results/recheck stack mise flox devbox devenv nix
python3 eval/harness/current-benchmark.py eval/results/recheck devenv-latest mise-configured flox-configured devbox-configured devenv-configured devenv-latest-dynamic
python3 eval/harness/compose-benchmark.py eval/results/recheck
python3 eval/harness/pixi-benchmark.py eval/results/recheck
python3 eval/harness/release-e2e.py eval/results/recheck
python3 eval/harness/release-native.py eval/results/recheck
python3 eval/harness/release-contract.py eval/results/recheck
python3 eval/harness/verify-comparison.py eval/results/recheck</pre>
<p>The receipt verifier checks the claimed successes, expected collision outcomes, marker separation, sixteen OS-specific scenario passes, lock/error probes, and the pilot’s task/failure totals. '''+link('verification.json','Verification receipt')+'''</p>
<p>All benchmark commands are local and reversible. No product code, commits, releases, pull requests, or external posts were created by this evaluation. New benchmark harness files and local results remain in the checkout.</p>
</main></body></html>'''
report=report.translate(str.maketrans({'“':'"','”':'"','‘':"'",'’':"'"}))
dest.write_text(report)
(base/'summary.json').write_text(json.dumps({'stack_version':'0.1.4','date':'2026-10-05','command_entry':measurements,'linux_scenarios':read('release-e2e/scenarios.json',[]),'sources':[{'url':u,'title':t} for u,t in sources]},indent=2)+'\n')
print(dest)
