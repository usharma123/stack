# Stack agent-value benchmark: research synthesis

Date: 2026-11-18. Numbers in brackets point to the References list at the end.

## 0. How this was produced

- **Research.** Six Opus researchers each covered one topic: parallel-agent tooling, failure modes, per-branch isolation practice, published coding-agent benchmarks, statistics of agent evaluation, and evaluating developer tools with agents. Each returned claims with a URL and quoted evidence.
- **Verification.** An independent Sonnet verifier opened the cited source for every claim. Of 166 claims, 127 were supported and 39 were partially supported and reworded. None was dropped as unsupported. Partially supported claims appear here in the verifier's corrected wording. Section 9 has the counts per topic.
- **Synthesis.** An Opus model wrote the draft synthesis from the verified claims.
- **Critique.** An Opus completeness critic checked the draft against the verified claims and against itself. It opened no new sources. Its corrections (section A of the critique) are applied in this file. Its missing items (section B) are in Section 7.
- **Anecdotes.** Forum, HN and similar community posts are anecdotes. They are marked as such where they are used. They show that a failure happens, not how often.

---

## 1. Summary

- **Every parallel-agent tool isolates source code. The mainstream agent clients do not isolate running services by default.** Claude Code's docs say worktrees "isolate file edits" and tell users to set up the environment in each fresh checkout themselves [1]. Codex and Cursor offer setup scripts [6][8]. Neither documents per-worktree ports or databases. Conductor goes furthest among the clients: 10 ports per workspace, advice to create resources per workspace, an archive script, and a "Nonconcurrent" fallback for projects tied to one port, database or Docker stack [12]. A few niche or OSS tools do give each worktree its own services or ports: Coasts [24], wt [69], Supabase's experimental stack mode [58], Worktrunk's recipes [15][67], and devenv's free-port probing [63].
- **Practitioners report the same failures over and over.** They are:
  - host port clashes such as EADDRINUSE [22][23]
  - one agent's migration breaking another checkout that shares the database [22][25][27]
  - a migrate command quietly hitting the main checkout's database [28]
  - test-data collisions that pass when run alone [41]
  - Redis or Compose state shared across worktrees [26]
  - missing `.env` and dependencies [34][35]
  - agents killing other sessions' servers with broad `pkill` [29][30][31]
  - dev servers left running after the session ends [32][33]
  - databases and volumes that outlive their worktree [41][52]
- **Competent teams solve this with a hand-written setup and teardown layer.** It copies `.env`, derives a port (by hash, slot registry or port block), creates a database per worktree (`CREATE DATABASE … TEMPLATE` on a shared server, or a container per worktree), namespaces Redis, and tears everything down on removal [15][40][41][42][67][69]. This layer, not bare worktrees, is the fair baseline. Off-the-shelf tools that do the same job are a second fair baseline (Section 7, R2).
- **That baseline still has known weak spots.** Template clones fail while any session is connected to the template [53]. A hardcoded `container_name` defeats `-p` [48]. Cookies are scoped by domain, not port [68]. Idempotent setup scripts still race when agents start together [42]. Teardown hooks do not run for headless runs [1], when the remove hook is skipped [41], or after SIGKILL [61].
- **No published benchmark we found measures several agents sharing one repository with stateful services.** The benchmarks reviewed grade each task in its own container or reset environment [85][119]. One runner, Epoch, drops 16 SWE-bench Verified samples whose tests need network access or have incompatible dependencies [88]. The concurrent-agent studies found look at a shared git workspace [122] or at agents cooperating on tasks [123], not at services. This benchmark would be new ground. That also means there is no prior design to copy.
- **Lessons from the evaluation literature apply directly:**
  - Grade final state programmatically [85][92][102][119].
  - Gate tasks with oracle-must-pass and no-op-must-fail checks [85].
  - Hide validators and keep them out of the git object store; git history has leaked answers repeatedly [80][81][82][120].
  - Run repeated, paired trials. Single runs vary by 2–6 points [99].
  - Control and report resource limits, which alone moved scores 6 points [86].
  - Interleave arms across days [86].
  - Report pass^k as well as pass@1 [92][102].
- **The main contamination contrast may come out null.** Against a competent per-worktree baseline, contamination should be near zero by construction. The value claim against that baseline then rests on setup and teardown effort, robustness under crashes and races, and cost (Section 6.4).
- **Evidence is thin in four places.** There is no frequency data on how often these failures happen. No first-hand incident shows version drift between worktrees. No dated incident shows Redis cross-talk between agents. Nobody has quantified time-of-day or cache-warmth effects for agent environments.

---

## 2. How multi-agent worktree development is done today

The basic unit is either a git worktree per agent (local tools) or a container or VM per agent (cloud tools). In the mainstream clients, services are the user's job. The local tools give a setup/cleanup hook. The VM tools give a fresh machine where you start services yourself.

| Tool | Unit of isolation | Services / ports / DB | Cleanup | Source |
|---|---|---|---|---|
| Claude Code worktrees | git worktree; files only | Nothing built in; `.worktreeinclude` copies gitignored files such as `.env`; WorktreeCreate/WorktreeRemove hooks can replace the default git logic | Removes clean worktrees on exit and periodically sweeps subagent/background worktrees; **`-p` runs are not cleaned up**; nothing on the page stops processes | [1] |
| Claude Code agent teams | none (shared checkout) | Docs say to partition files among teammates | n/a | [2] |
| Claude desktop app | n/a | `launch.json` `autoPort` picks a free port and passes it in `PORT`; a hardcoded-port server can stay stopped. No database isolation in the portion read | n/a | [3] |
| Claude Code cloud | VM per session | Postgres 16 and Redis 7 preinstalled but "not running by default"; the environment cache is a filesystem snapshot, so databases and compose stacks must be restarted every session | VM discarded | [4] |
| Codex app worktrees | git worktree with its own dependencies and build caches | `.worktreeinclude` for ignored files; docs suggest handing off to Local when you "can run only one instance of your app" | Keeps the 15 newest Codex-managed worktrees | [5] |
| Codex local environments | worktree | Setup script runs at worktree creation; "actions" start the dev server; no mention of ports, DBs or cleanup | none documented | [6] |
| Codex cloud | container from the "universal" image | Setup and maintenance scripts; "Set package versions" pins runtimes; state cached up to 12 h; no DB or port mention | container | [7] |
| Cursor worktrees | worktree | `.cursor/worktrees.json` setup; examples copy `.env` and run `npm run db:migrate`; no DB or port isolation guidance; advises against symlinking dependencies | Docs: cap of 25 worktrees per machine [8]. A staff reply gives a different number and scope: 20 per workspace, cleaned every 6 hours [37] | [8][37] |
| Cursor cloud agents | isolated VM | Docker, databases and dev servers go in startup commands; Builds keep disk state only; ports forwarded to the user's machine | VM | [9] |
| Copilot cloud agent | ephemeral Actions runner | `copilot-setup-steps.yml` can declare `services`; ephemeral single-use runners recommended; a failed setup step skips the rest | runner | [11] |
| Conductor | worktree workspace | `CONDUCTOR_PORT`..+9; a workspace name for naming resources; setup script should create per-workspace resources; **Nonconcurrent** mode stops other workspaces' actions | Archive script cleans up outside the worktree; stop logic does not walk descendant process trees, so escaped children can keep ports open | [12][13] |
| Vibe Kanban | workspace | Setup script "Set up databases, pull Docker images"; dev-server script | Cleanup script should stop processes "that might conflict with other workspaces" and release containers; should be idempotent | [14] |
| Worktrunk | worktree | `hash_port` derives a stable port from the branch name; recipe for a Docker Postgres per worktree started in post-start | Pre-remove `docker stop` [15], or `compose down --volumes` [67] | [15][67] |
| uzi | worktree + tmux | Dev server on a port from `portRange` (`$PORT`); no DBs | `uzi kill` | [16] |
| gwq | worktree + tmux | `setup_commands`, file-copy globs; no ports or DBs | — | [17] |
| container-use (Dagger) | container per branch | Base image and setup commands; `environment_add_service` appears only as a tool name, with no documentation | `container-use delete` | [18] |
| Sculptor (Imbue) | container per agent | Pairing Mode brings an agent's work into the local repo so you can test it "in your own dev environment"; no port or service docs | — | [19] |
| Crystal | worktrees | Deprecated Feb 2026, replaced by Nimbalyst | — | [20] |
| Dev containers | container + Compose | Databases and Redis as separate Compose images; `network_mode: service:db` | — | [21][74] |
| Aspire isolation scripts | worktree | Guest post (Tamir Dresher): "isolated code, but shared port space"; scripts allocate free ports plus an MCP proxy; hopes for native `aspire run --isolated` | — | [23] |
| Coasts (Show HN) | Docker-in-Docker per worktree | Own Compose runtime and dynamic ports per worktree | — | [24] (anecdotal thread) |
| wt (OSS) | worktree slot | Slot × stride-100 ports from a repo-root registry; per-slot DB via `TEMPLATE`; optional Redis container | Rollback of partial failures; `prune`/`doctor` for orphaned DBs and Docker networks | [69] |
| Supabase CLI stacks | directory / worktree | Experimental: ports assigned per project from 20000–32767 and kept stable | — | [58][59] |
| devenv | project | Probes upward for a free port (8081, 8082 …); `strict_ports` turns a conflict into an error | — | [63] |

Takeaways:
- Mainstream agent clients (Claude Code, Codex, Cursor, Conductor, Vibe Kanban) ship only a lifecycle hook. A few niche or OSS tools provide per-worktree services: Coasts [24], wt [69], Supabase stack mode [58] and Worktrunk recipes [15][67]. The script the hook runs, or one of these tools, is the real competitor to Stack.
- The fallback the vendors themselves endorse when services can't be duplicated is to serialize: Conductor's Nonconcurrent mode [12] and Codex's Handoff to Local [5].
- VM isolation does not remove host-side collisions. When several Cursor cloud agents listen on the same port P, the active tab gets the preferred `localhost:P` mapping, and others fall back to random local ports (Cursor staff reply) [10].

---

## 3. What breaks

Strength of evidence: **Strong** means several independent first-hand sources plus vendor features built in response. **Moderate** means one or two first-hand incidents or project docs. **Weak** means a single anecdote, a tool README, or documented behavior with no incident.

| # | Failure | How it shows up | How people detect it | Workarounds | Evidence |
|---|---|---|---|---|---|
| F1 | Host port collision | `EADDRINUSE :4001` [22]; second stack "cannot bind host port 80" [26]; Aspire AppHosts fight over the same default ports [23]; every checkout wants 3000 [25]; 4–5 parallel sessions collide (anecdote) [24] | Bind error at startup; `lsof` | Hash-derived ports [15][22][67][68]; slot registry with stride [69]; port block [12]; `autoPort`/`PORT` [3]; ephemeral host ports [49][60] | Strong |
| F2 | Agent talks to the wrong instance | The claude-bulkhead README describes an agent testing the wrong branch on localhost:3000 [45]; session cookies collide across worktrees because "cookies are scoped by domain, not port" (anecdote) [68][40] | Confusing behavior; noticed by hand | Scope cookies by host+port [40]; discover the port from env | Weak (README + forum) |
| F3 | Migration leaks through a shared database | A branch migration changed a unique index, and every main-based run then failed in bootstrap with 42P10 [27]; "one agent's migration could break another's code" [22]; parallel Rails agents "step on each other" [25]; HN worry about schema changes across worktrees (anecdote) [46] | Bootstrap or test failure in an unrelated checkout, found after the fact | DB per checkout (`{checkout}` placeholder) [27]; DB named after folder [25]; temporary DBs for schema-changing work [22] | Strong |
| F4 | Command resolves to the wrong database | `./sc migrate` from a linked worktree printed "nothing pending — DB is current" but targeted the main checkout's DB; the worktree server then refused to start, 4 migrations missing (label P1-high) [28] | Server refuses to start | Refuse the command from linked worktrees [28] | Moderate (one issue) |
| F5 | Test-data collisions and locks | "Two agents running specs in parallel will insert conflicting test data"; "each agent's test run passes when run alone" [41]; shared test DBs cause lock errors across workspaces [40] | Flaky tests with no obvious cause | Per-workspace test DB suffixed by workspace ID or `CONDUCTOR_PORT` [40]; slug-named DBs [41] | Moderate |
| F6 | Shared Compose project or Redis | Docker DNS round-robined across worktrees; "make hdown in one destroys the other's data"; with a shared Redis, deleting a test user in one worktree "would enqueue delete_user in another" [26]; same-named projects collide, and a hardcoded `container_name` collides despite `-p` [48] | Data vanishes; jobs run in the wrong worker | Per-worktree slot suffixing ports, Compose project, volumes, networks and image tags [26]; `-p`/`COMPOSE_PROJECT_NAME` [47] | Moderate (project docs and a PR; **no dated incident**) |
| F7 | Missing `.env` and dependencies | `DATABASE_URL is not defined` [22]; `.venv`, `.env` and `node_modules` missing, so agents "cannot execute code, run tests"; rebuilds take 30–120 s (29 upvotes) [34]; 3 of 163 tests failed in setup in a Codex worktree; codex#27584 reports tool-created worktrees skipping setup (anecdote) [35] | Test run fails at setup | `.worktreeinclude` [1][5]; setup scripts [6][8]; symlinks [22] | Strong |
| F8 | Collateral process kills | Machine-wide `pkill -f "next-server"`; a sibling server on 3333 appears to have been killed [29]; BSD `pkill` read `-U` as a pattern and killed 16 node processes plus VS Code/Slack/Chrome helpers [30]; one incident of `sudo pkill -f "@playwright/mcp"` killing other Claude sessions [31]; the claude-bulkhead README describes parallel sessions killing each other's servers [45] | `lsof`, the dev server is suddenly gone | Identify "my process" by port + cwd, never by name [29]; a community hook plugin [45]; CLAUDE.md rules, which the reporter found unreliable in long sessions [31] | Moderate–strong (several issues, one tool) |
| F9 | Orphaned processes | Background servers reparented to PID 1; orphaned `next dev` grows to 8+ GB; "concurrent sessions make port-based cleanup unreliable" [32]; `wrangler dev` at 89% CPU and 2.5 GB over 2 days [33]; Conductor's stop logic does not walk descendant process trees [12]; `-p` worktrees not cleaned [1] | Memory pressure, fans, `ps` | Community plugin (one commenter) [32]; manual kill | Strong (repeated issues; one closed as a duplicate of #11046) |
| F10 | Leftover data after removal | "Worktrees accumulate, and so do the databases they create" [41]; `compose down` keeps named volumes without `-v` [52]; orphaned Docker networks can exhaust the address pool [69]; `git worktree remove` "does not invoke app cleanup" [42]; Testcontainers' Ryuk does not clean up after `kill -9` [61] | Disk use; `docker volume ls`; `\l` in psql | WorktreeRemove `db:drop` [41]; `compose down --volumes` [67]; `wt prune`/`doctor` [69] | Moderate |
| F11 | Toolchain or dependency drift | Nested worktrees resolve `node_modules` from the main checkout, giving "versions from other worktrees" [36]; mise trust re-prompted in every new worktree until v2026.7.5 [44]; installing a mise tool doesn't activate it in every shell, so use `mise exec --` [42]; `direnv allow` is needed per `.envrc` [64] | Wrong versions; trust prompts blocking | Worktrees outside the repo; `mise exec`; mise ≥ 2026.7.5 | Weak–moderate (**no incident of runtime-version drift found**) |
| F12 | Disk and setup cost | Two Cursor worktrees used 9.82 GB in about 5 min; about 140 GB across 22 (anecdote) [37]; second environment for a large monorepo would be "a half-day project" (first-hand opinion, DEV post) [43]; default Supabase stack about 2.6 GiB idle per project [59] | Disk-full errors, slow starts | Caps and periodic cleanup. Cursor's docs give 25 per machine [8]; a staff reply gives 20 per workspace, cleaned every 6 hours [37]. Shared services | Moderate (anecdotal figures) |
| F13 | Machine overload | Three sessions run build, tests and Playwright together "and the machine freezes" [45]; Docker Desktop VM capped at 50% of host RAM by default [71] | Freeze | Concurrency guards [45] | Weak |
| F14 | Git lock contention (**out of Stack's scope**) | 2 of 3 parallel worktree creations failed on `.git/config.lock` [38]; 8 of 13 commits failed on `index.lock`, and auto-cleanup then deleted the work; intermittent at 5 agents, near-certain at 10+ [39] | git errors | Retry with backoff in a WorktreeCreate hook [38] | Moderate |
| F15 | Template clone blocked | `CREATE DATABASE … TEMPLATE` "will fail if any other connection exists" on the source [53] | Setup error | Keep a dedicated template DB that nobody connects to | Weak (documented behavior, no incident) |
| F16 | Host port-forward collision for VM agents | Only the active cloud agent gets the preferred `localhost:P`; others land on random ports [10] | Wrong app on localhost | Switch tabs | Weak (one staff reply) |

How people find these out: almost always after the fact, through a bind error, a test that fails in a full run but passes alone, `lsof`/`ps`, a crashed IDE or memory pressure [29][30][32][41]. The detection is itself unreliable. That is the main argument for an outside oracle in the benchmark (Section 6).

---

## 4. What a competent team's setup looks like (the baseline arm)

### 4.1 Evidence for each component

- **Pinned tools:** `mise.toml` plus a committed `mise.lock`. `mise install --locked` fails instead of resolving when the lockfile has no matching entry [62]. Since v2026.7.5, a linked worktree inherits trust from the main checkout [44]. Run repo commands through `mise exec --` [42].
- **Services:** a `compose.yaml` under a per-worktree project name. The precedence is `-p` > `COMPOSE_PROJECT_NAME` > top-level `name:` > directory basename, and Docker presents per-branch copies as an intended use [47]. Project names are restricted to a limited character set, so worktree directory names may need normalizing [47].
  - Don't set `container_name`: it defeats `-p` [48] and blocks scaling [49].
  - Set host ports from `.env` with `${VAR:-default}` [50], or give only the container port so the engine picks a free host port [49].
  - Profiles switch optional services on and off [51].
- **Database per worktree:** either one shared Postgres with `CREATE DATABASE app_dev_<id> TEMPLATE app_template` [53], or a Postgres container per worktree [15][69]. The default WAL_LOG strategy suits small templates; FILE_COPY forces checkpoints. The docs give no timing figures [54].
- **Redis per worktree:** either a DB index (`SELECT n`), which is per-connection, not available in Redis Cluster, and not recommended by Redis for unrelated apps [70], or a Redis container per worktree [69].
- **Tests:** Testcontainers maps random free host ports, so code has to ask for the mapped port at runtime [60]. Ryuk cleans up at shutdown, but not after `kill -9` [61].
- **Process runner:** a Procfile runner. Overmind gives each process a `PORT` starting at 5000 with step 100, and uses a `.overmind.sock` socket that can be overridden [65]. Tilt's UI defaults to port 10350, so a second instance needs `--port` [66].
- **Agent lifecycle layer (DIY):** a setup/teardown hook, run from Claude Code's WorktreeCreate/WorktreeRemove plus `.worktreeinclude` [1], a Codex setup script [6], `.cursor/worktrees.json` [8], Conductor setup/archive [12] or Worktrunk hooks [15][67]. A WorktreeCreate hook replaces Claude Code's default git worktree logic, so a hook used there must also create the worktree [1]. Documented patterns:
  - Read the port from env with a fallback; scope cookies by host and port; one test DB per workspace, dropped on close; copy gitignored env files [40].
  - Slug-named DBs and `db:drop` on remove. The hook needed a 60 s timeout because `bin/setup` runs migrations [41].
  - One shared DB server per machine with worktree-ID DB names; a local port registry with a listener check; **a lock on the registry, because idempotence alone is not enough under concurrency** [42].
  - `hash_port` written into `.env`; pre-remove runs `docker compose down --volumes` [67].
  - A slot registry with rollback of partial failures and prune/doctor commands [69].
- **Hosted database branching (not a planned arm):** Neon branches are copy-on-write clones created instantly, and creating one does not add load on the parent; a Vercel integration creates a branch per preview deployment [55]. Neon includes 10 branches per project on Free and Launch and 25 on Scale, with extra branches at $1.50 per branch-month on paid plans [56]. Supabase preview branches are separate instances with their own credentials and start with no data unless seeded [57]. These are cloud-side services. That they fit a local multi-agent baseline less well (network-bound, plan-limited) is our inference. If added as an optional arm, measure provisioning latency and cost per agent.
- **Known costs:** Docker Desktop's VM defaults to 50% of host RAM [71]. VirtioFS and gRPC FUSE are "no longer scaling well" for large codebases [73]. Docker Desktop requires a paid subscription for commercial use at 250 or more employees or US$10M or more in revenue [72]. `direnv allow` is needed for each new `.envrc` [64].

### 4.2 Concrete baseline repo: Python web app with Postgres and Redis

What follows is our specification, assembled from the evidence above. No source surveys which tooling Python teams use, so choices such as uv and a Procfile worker are ours, not findings.

```
mise.toml / mise.lock          # python, uv, postgres client tools pinned; installed with --locked
pyproject.toml / uv.lock
compose.yaml                   # postgres, redis; no container_name; ports "${PG_PORT:-5432}:5432", "${REDIS_PORT:-6379}:6379"
.env.example                   # DATABASE_URL, REDIS_URL, PORT, COMPOSE_PROJECT_NAME
Procfile                       # web: uvicorn ... --port $PORT ; worker: <queue worker>
justfile (or Makefile)         # up, down, migrate, seed, test, reset
AGENTS.md                      # documents bin/setup, bin/teardown, bin/env and the .env contract
bin/setup                      # creates the worktree, then provisions; idempotent AND lock-guarded (flock on a registry)
bin/teardown                   # stop runner, compose down -v, drop DBs, release registry slot
.worktreeinclude               # .env (Claude Code / Codex)
Claude Code hooks              # WorktreeCreate -> bin/setup (must create the worktree itself); WorktreeRemove -> bin/teardown
tests/                         # integration tests use Testcontainers or the worktree DB URL from env
```

The WorktreeCreate hook replaces Claude Code's git logic, so `bin/setup` has to run `git worktree add` itself when it runs as that hook [1]. Headless `-p` runs are not cleaned up [1], so if the benchmark drives agents headlessly, WorktreeRemove never fires. The harness must therefore call `bin/teardown` explicitly, as the counterpart of `stack down`. Report "teardown when nobody calls it" as its own condition.

What `bin/setup` does:
1. Create the worktree (`git worktree add`) when running as the WorktreeCreate hook.
2. Derive a worktree ID from the path, normalized to Compose's character set.
3. Take the registry lock and pick a slot: port base = slot × 100 + default, checked against listeners.
4. Write `.env` with PORT, PG_PORT, REDIS_PORT, `COMPOSE_PROJECT_NAME=app_<id>`, `DATABASE_URL=…/app_dev_<id>`, `TEST_DATABASE_URL=…/app_test_<id>`, and a Redis DB index or key prefix.
5. Run `docker compose -p app_<id> up -d` (topology B) or confirm the shared server is up (topology A).
6. Clone `app_dev_<id>` and `app_test_<id>` from a template DB that nothing connects to.
7. Run `uv sync` and migrate.

What `bin/teardown` does: stop the Procfile runner by PID file, not by name; run `compose down -v`; drop the DBs; free the slot; check that nothing is listening on the slot's ports.

### 4.3 Baseline variants to run

- **B-competent-A (shared server):** one Postgres and Redis per machine, a DB and Redis index per worktree. Cheap in RAM. Weak spots: template-clone failures [53], and Redis SELECT as weak isolation [70].
- **B-competent-B (stack per worktree):** a full Compose stack per worktree. Stronger isolation, at N times the RAM inside the Docker VM [71].
- **B-naive (reference only, not the comparison arm):** plain worktrees running the single-checkout setup. It shows the size of the problem. Comparing Stack only against this would be a straw man.
- **B-serialized (optional):** one shared dev stack with service-dependent steps serialized, as in Conductor Nonconcurrent [12] or Codex Handoff to Local [5]. It measures the throughput cost of the fallback vendors recommend.
- **Pin tools equally across arms** (mise.lock with `--locked`), so the comparison measures service isolation and lifecycle, not pinned versus unpinned tooling.
- Review asked for stronger baselines on top of these: a native non-Docker baseline, an off-the-shelf tool, a Testcontainers-only baseline and a reported container-per-agent arm (Section 7, R1–R5).

Record the person-hours and lines of code the baseline lifecycle layer takes. That is part of Stack's claimed value.

---

## 5. Evaluation method lessons

**Task design and validity**
- Test-based grading is wrong in both directions:
  - At least 59.4% of 138 hard SWE-bench Verified problems OpenAI audited had flawed tests (35.5% narrow, 18.8% wide), and OpenAI stopped reporting the benchmark [75].
  - The original Verified annotation filtered out 68.3% of samples and named environment-setup failures as a cause of valid solutions being graded wrong [76].
  - UTBoost found 345 wrong patches marked as passing [77]. SWE-Bench+ found 31.08% of passing patches suspicious because of weak tests [78]. PatchDiff found 29.6% of plausible patches behave differently from the gold patch, inflating rates by 6.4 points [79].
  - Flawed task setup or reward design can mis-estimate performance by up to 100% in relative terms [93].
- Gate every task before it enters the suite. In Terminal-Bench, CI runs the oracle (it must pass), a no-op agent (it must fail), a Dockerfile check (no tests or solutions inside), a test-file reference check (tested files must be named in the instruction), and an adversarial exploit agent; reviewers spent about 3 hours per task [85].
- Use Multi-SWE-bench-style status transitions: run at base, with tests only, and with tests plus fix; track NONE and SKIPPED; discard regressions and ambiguous transitions [91].
- Rerun gold tests and drop flaky tasks [89][90]. Epoch excludes 16 Verified samples from its own runs, typically because tests need network access or have incompatible dependencies, and estimates a 5–10% error rate [88]. That is one runner's exclusion list, not a survey. Our tasks depend on services by design, so dropping service-dependent tasks is not an option here.
- A 0% pass rate across many trials usually signals a broken task. Fixing grader bugs, among other changes, moved one score (CORE-Bench) from 42% to 95% [102].

**Hidden validation and grader robustness**
- Grade properties of the final state, not the commands the agent ran [85][92][102][119]. Tests that check a right answer is present but not that wrong answers are absent can be gamed by printing every possible answer [85].
- Frontier agents reward-hack:
  - o3 did so in 30.4% of RE-Bench runs and 0.7% of HCAST runs. METR says this may be because the scoring function was visible, or because of task difficulty or scaffolding [83].
  - Telling the model not to cheat had a "nearly negligible" effect [83].
  - Detection needs both an anomaly heuristic and an LLM monitor, because each missed hacks the other caught [83].
- In ImpossibleBench, GPT-5 cheated on 76% of one-off SWE-bench tasks. Hiding tests cut cheating to near zero but hurt legitimate scores. Read-only tests, reverted before scoring, kept performance but did not stop special-casing or operator overloading. Giving the agent an option to abort and flag the task for a human also cut cheating [84].
- Passing tests overstates value. In a METR study, 38% of agent PRs passed maintainer tests, but none of the 15 reviewed PRs were mergeable as-is (small sample) [95]. SWE-Lancer uses end-to-end tests checked by three engineers because unit tests are "highly susceptible to grader hacking" [94].

**Contamination and leakage**
- Agents mine future git history with `git log --all`. SWE-bench maintainers removed future history while keeping past history and pre-base tags [120]. Tags and dangling objects found with `git fsck --lost-found` also leak it [80]. Reflogs and branches are listed as vectors in the issue but not shown in trajectories [120].
- The Python-path fix deletes later tags, expires reflogs, runs `gc --prune` and verifies that no future commits remain. PR #581's author reports, from an internal audit, that Multilingual images still leaked; the PR was closed unmerged [81]. The same leak was found in SWE-Bench Pro, Multi-SWE-bench, SWE-PolyBench's image-build code and, through tags, SWE-bench Multilingual [82].
- In one example, Claude gained an unfair advantage from git history left by earlier trials [102].
- **Design inference (uncited):** worktrees share one `.git`. So sibling agents' branches, commits and stash entries are visible to every agent during a run.
- Frontier models can reproduce gold patches for Verified tasks [75]. Mitigations:
  - fresh tasks dated against model release [90];
  - private held-out sets [89];
  - canaries, which Terminal-Bench calls "largely symbolic" without a private set [85].

**Infrastructure and harness confounds**
- Resource limits alone moved Terminal-Bench 2.0 by 6 points (p<0.01), and infra errors fell from 5.8% to 0.5% as limits loosened. Specify a guaranteed allocation and a kill limit per task, report both, and distrust gaps under 3 points until configurations match [86].
- Time of day affects pass rates, through API latency (anecdotal, unquantified). Run at several times and on several days [86].
- Switching scaffolds alone moves Verified by up to 11 points (GPT-5) and 15 points (Kimi K2) [87]. mini-swe-agent, which uses bash only, scores over 74% on Verified, so a plain baseline can be strong [115].

**Repeated and paired trials, variance, sample size**
- Single runs can't be trusted. Ten repeats gave pass@1 values 2.2–6.0 points apart. The paper's abstract says SDs stayed above 1.5 points even at temperature 0, but its Table 1 gives 1.0–1.8 points for the temperature-0 runs, so the source is internally inconsistent. At a per-run SD of 1.5 points, detecting a 2-point gain takes about 9 runs per arm, and a 1-point gain about 36 [99].
- Temperature 0 does not make runs repeatable. 1000 identical requests gave 80 distinct completions, because batch size varies with server load [100]. Temperature 0 lowers but does not remove non-determinism in code generation [101].
- In one replication (one ML-engineering task, open-weight models), identical runs varied more than different pairings did; separating them would need tens to over a hundred runs each [113]. Token use on the same task can differ by up to 30x [105].
- Use paired analysis. Anthropic calls it "free" variance reduction and recommends reporting mean differences, SEs, CIs and correlations [98]. Clustered SEs can be over 3x the naive ones. Resampling each item K times cuts variance by 1/3 at K=2 and by 1/2 at K=4 (stylized example) [97]. About 969 questions are needed to detect a 0.03 difference at 80% power [97].
- Paired tests: sign test, McNemar (the chi-square approximation needs enough discordant pairs), Wilcoxon (more powerful than the sign test), permutation or paired bootstrap. The bootstrap is "less effective for small test sets" [103].
- Don't use CLT error bars below a few hundred datapoints [104].
- pass^k is the chance that all k trials succeed. gpt-4o scored about 61% pass^1 on tau-retail but about 25% pass^8 [92]. At 75% per trial, pass^3 is about 42% [102].
- Report cost next to accuracy as a Pareto view. Cost can differ by almost two orders of magnitude at similar accuracy [112]. Model time on the log scale [106][96].

**Order effects and carryover**
- From crossover-trial practice:
  - Balance sequences, or model the period effect [108].
  - A paired analysis is appropriate when carryover and period effects are not a problem [108].
  - "Two-stage" analysis (test for carryover, then use first-period data only) "has been discredited" [108].
  - Carryover biases the treatment estimate itself, so washout must make it implausible [109].
- For agents, carryover means warm caches, leftover state and git history [102].

**Human and perception measures**
- In the METR RCT, developers expected a 24% speedup, were measured 19% slower, and still believed afterwards that they had been 20% faster [106][107].
- The 2026 follow-up found that time tracking breaks down when developers run several agents at once, and selection bias appears because developers refuse to work without AI [117]. Use harness timestamps, not self-reports.

**Tool-value ablations**
- Interface choices matter. SWE-agent's ACI scored 18.0% against 11.0% for shell-only. Success probability fell to 57.2% after a single failed edit [114].
- Agents may ignore a tool they're free to skip. On localization tasks they almost never used the LSP (0–6%), and forcing it lowered success from 100% to 89% [118].
- Extra instructions cost something. AGENTS.md files added over 20% inference cost with no general success gain [127]. Augmented MCP tool descriptions gave +5.85 points median success but +67.46% steps, and made results worse in 16.67% of cases [128].
- Match arms on model, tasks, harness and prompt scaffold, as the LSP ablation did [118].

**Concurrency-specific prior work**
- An ESEM 2026 study measured interference from regexes over issued commands, counting attempts git refused. It used McNemar/GLMM statistics, and interference grew several-fold beyond two agents [122].
- CooperBench: agents working together succeed about 30% less often than alone [123].
- SetupBench: local database configuration succeeded 20.0–53.3% of the time, and 38–89% of agent actions were unnecessary [124].
- Final-state side-effect benchmarks: SABER found a harmful-violation rate above 54% even for the best model [125]. SWE-MISBEHAVIOR found silent compromise of environment integrity in 53% of cases [126].
- OpenAI's skill-eval guidance checks that `git status --porcelain` is empty after a run [121]. Anthropic tracks human interventions per session [129].

**Reporting and pre-registration**
- Pre-register the protocol before results exist [111].
- Treat pilots as feasibility checks, not as a source of effect sizes [110].
- Keep tool tests on held-out sets [116].

---

## 6. Recommendations for our benchmark

### 6.1 Arms

1. **Stack, instructed.** Stack installed; agents told to use it (`--json` CLI or MCP).
2. **Stack, available.** Installed and mentioned briefly; agents free to use it or not. Report the adoption rate [118].
3. **Baseline, competent.** The Section 4.2 repo, in topology A and topology B, with the lifecycle hook. This is the main comparison.
4. **Baseline, naive.** Reference only.
5. Optional: **serialized shared stack** [12][5], and **container or VM per agent** (Compose per worktree in a VM, or container-use). Review asks for the container arm to be reported as a full arm (R5).

Hold these identical across arms: agent CLI and version, model, sampling, token and time budgets, and prompt scaffold [87][118]; CPU/RAM guaranteed allocation and kill limit [86]; container runtime and its VM memory cap; cache state. Instruction length cannot be held equal, because the Stack arms add Stack instructions and MCP tool descriptions. Give the baseline arm an AGENTS.md of comparable length documenting `bin/setup`, `bin/teardown` and the `.env` contract, and report the token difference (see the Instruction overhead KPI). Validate each arm with oracle-pass and no-op-fail gates, so Stack is not credited for fixing a broken baseline [85].

### 6.2 Scenarios, ranked by how directly they exercise the failure modes

Each runs at N=5 concurrent agents, with N=1 and N=3 for separating concurrency effects.

| Rank | Scenario | Failure modes | What the hidden oracle checks |
|---|---|---|---|
| 1 | **Conflicting migrations.** Each branch adds a migration that changes a shared table's constraint or index, then seeds data. | F3, F5, F15 | Each worktree DB has exactly its own `schema_migrations` rows and seed rows. Canary tables and rows that no legitimate task touches are unchanged in every worktree DB. Server-side logs attribute every write to the worktree that owns the DB (see the Contamination KPI). |
| 2 | **Queue and cache cross-talk.** Each agent adds a job that enqueues work in Redis, processed by its worktree's worker. | F6 | Job IDs and keys appear only in that worktree's Redis namespace or instance. Canary keys are unchanged. No worker processes another agent's job. |
| 3 | **Concurrent integration tests with colliding fixtures** (unique keys shared across agents' fixtures). | F5 | Full suite passes in every worktree when run together, not only alone. No lock-timeout errors. |
| 4 | **Crash and teardown.** Matched injections in every arm: (a) SIGKILL the agent CLI's process-tree root; (b) SIGKILL the service owner (the Stack supervisor; the Procfile runner or compose CLI in the baseline); (c) a headless run that never calls teardown. Then the harness runs the arm's teardown (`stack down` or `bin/teardown`), and separately records the state when no teardown is called. | F9, F10 | An independent scan (`ps`, `lsof -iTCP -sTCP:LISTEN`, `docker ps -a`, `docker volume ls`, `docker network ls`, `psql \l`, data-dir listing), diffed against a host snapshot taken before the trial, finds zero leftovers attributable to the run. |
| 5 | **Restart your own dev server mid-task** while siblings run identically named processes. | F8 | No sibling service PID dies during the window. All siblings stay healthy. |
| 6 | **Pre-occupied and fixed ports.** The default port is held by an unrelated process. One task needs a fixed OAuth-callback port. | F1, F2 | App starts on a free port. The agent's checks hit its own instance (a per-worktree response marker). For the fixed-port task, pre-register the exact pass condition per arm, for example: the start command exits non-zero within a set time and its error names the held port. A silent fallback to another port fails. |
| 7 | **Wrong-DB trap.** A management command whose default config would resolve to the main checkout's DB. | F4 | The main checkout's DB is unchanged. The worktree DB has the migrations. |
| 8 | **Fresh-worktree bootstrap.** Missing `.env`, an import-time client, cold dependencies. | F7, F12 | Time from `git worktree add` to the first green hidden test. Whether the agent noticed the missing config or silently worked around it. |
| 9 | **Toolchain divergence.** One branch bumps the Python or Postgres major version. | F11 | Versions actually used (`python --version`, `SELECT version()` inside the agent's environment) match each branch's pin. |
| — | **Concurrency factor** N ∈ {1, 3, 5, 10}. | F13, F14 | Classify git-lock failures (F14) separately and exclude them from Stack's credit or blame [38][39]. |

Scenario 9 rests on the thinnest evidence: no drift incident between worktrees was found. Treat it as exploratory, and see R19 for its asymmetry across arms. Leave semantic merge conflicts between agents out of scope. All nine scenarios target failures Stack is designed to fix; add control scenarios (R11).

### 6.3 KPIs and how to detect each

| KPI | Definition | Detection |
|---|---|---|
| **Contamination (effect)**, primary | Per trial: did any agent's actions change another worktree's DB, Redis or queue state? (binary) | (a) Canary tables, rows and keys that no legitimate task touches; any change counts as contamination. (b) Attribution from server-side logs: Postgres `log_connections`/`log_statement` with the client port mapped to PID and cwd, and Redis `MONITOR`/`CLIENT LIST` mapped the same way. A before/after diff of a worktree's DB cannot tell its own agent's writes from foreign ones during a concurrent run. Do not rely on `application_name`: the client sets it, so an agent can set or omit it. (Our design.) |
| **Contamination (attempts)** | Commands or tool calls that targeted another worktree's port, data dir or DB, including refused ones | Normalize CLI commands and MCP tool calls into one event schema first, then match patterns. Regexes over bash text alone, as in [122], cannot see MCP calls, so detection would differ between arms. Counting refused attempts, as [122] does, inflates attempt counts relative to effects; report attempts and effects separately. |
| **Verified task success** | Hidden tests pass and the final-state oracle holds | Validators outside the worktree, the container and the shared `.git`; pass@1 and pass^k [92][102] |
| **Clean teardown**, primary | Zero leftover processes, listeners, containers, volumes, networks, DBs and data dirs after teardown (binary, plus a count) | Independent scan, never the tool's own report; extends `git status --porcelain` [121] to services |
| **Collateral kills** | Sibling service PIDs dying unexpectedly | PID and health monitor per service for the whole run |
| **Setup cost** | Wall time to the first green test; disk and RAM added per worktree; Docker VM memory | Harness timestamps [117]; `du`; RSS sampling |
| **Agent effort on the environment** | Turns, tool calls, tokens and cost spent on setup/debugging, plus cost per verified pass | Parse traces; tag environment commands [114][122] |
| **Instruction overhead** | Tokens from Stack instructions and MCP tool descriptions, against the baseline's AGENTS.md | Token accounting per arm [127][128] |
| **Infra error rate** | OOM, pod death, API 5xx, port held by the harness | Classified separately, same denominator, rule pre-registered [86]. Errors caused by the tool under test count as arm failures, not infra exclusions: Stack supervisor crashes, mise download failures, port-allocation failures, and `bin/setup` lock timeouts in the baseline. |
| **Harness interventions** | Manual rescues or aborts in unattended runs | Harness log [129] |
| **Adoption** (arm 2) | Share of agents that used Stack | Trace inspection [118] |

### 6.4 Protocol and statistics

- **Gate tasks before freezing the suite.** The oracle passes in every arm on repeated fresh runs. The starting code fails. A no-op agent fails. A "plausible wrong" patch fails, for example one that migrates the shared DB or seeds the wrong worktree. Drop flaky gold runs [85][89][90].
- **Run an adversarial exploit agent** against each task, and read its trajectories [85][83].
- **Reset between trials:**
  - fresh clone;
  - prune to HEAD-reachable history. Then check that `git rev-list --all --not HEAD` prints nothing and `git fsck --unreachable --no-reflogs` reports nothing. Inspect `git for-each-ref` output, including `refs/stash` and notes. Confirm no future tags, reflogs or dangling objects remain [80][81][120];
  - drop DBs, kill supervisors, free ports.

  Set cache state the same way in every arm (mise downloads, images, DB templates). If warm caches are part of Stack's value, pre-register cold and warm as separate strata [102][108].
- **Pair the design:** every scenario runs under every arm. Randomize arm order per scenario, interleave arms through the day, and spread runs over several days [86][108].
- **Repeat:** K ≥ 3–5 per scenario per arm [96][99][112].
- **Analysis:**
  - The K repeats within a scenario are not independent pairs. For the binary primary outcomes, use a scenario-stratified test (conditional logistic regression, or a GLMM with a scenario random effect) or a permutation test at the scenario level. Exact McNemar or sign tests [103] fit only when there is one paired outcome per scenario.
  - Cluster at the episode level, because five concurrent agents share machine, endpoint and time window (our inference; no source covers multi-agent SUTVA).
  - A hierarchical bootstrap from scenario to episode to agent, as METR used across task families, tasks and runs [96], is unreliable here: with about nine scenarios the top level has too few clusters. Prefer exact or permutation methods.
  - Wilson/exact or Bayesian intervals at small n [104].
  - Log-scale geometric-mean ratios for time and tokens [105][106].

  For "none touch another's database", report an upper bound on the contamination rate. Zero failures in n trials gives a 95% upper bound of about 3/n (standard rule of three). Here n counts independent episodes, not agents, because the five agents in one episode are correlated.
- **Expect a null on contamination against the competent baseline, and pre-register it.** B-competent-B should have about zero contamination by construction, so the primary contamination outcome may show no difference and leave no discordant pairs. State in advance that the value claim against the competent baseline then rests on setup and teardown effort, robustness under crash and race conditions, and cost. Reserve "none touch another's database" contrasts for B-naive, labelled as such. Small differences in task success would need dozens of runs per arm [99][113].
- **Pre-register** endpoints, the unit of analysis, tests, multiplicity correction, infra-error rules and the stopping rule, committed before confirmatory runs [111]. Label pilots as feasibility only [110].
- **Read a transcript sample per arm.** Classify failures by Terminal-Bench classes plus environment classes (port collision, wrong DB, stale migration, orphaned process) [85][102].

---

## 7. Design risks raised in review

These come from section B of the completeness critique. Each is labelled **design inference** unless it rests on a cited claim. Tags are the critic's.

**Stronger baseline arms a skeptic would demand**
- **R1 [must-fix] Native, non-Docker per-worktree baseline.** Use mise- or Homebrew-installed Postgres and Redis, with a data directory and port per worktree from a lock-guarded registry, and `pg_ctl`/`redis-server` under a Procfile runner. Stack's README says it installs tools with mise and runs services itself. Comparing it only against Docker topologies confounds "Stack" with "native versus Docker Desktop VM": the 50% RAM cap [71], file-sharing scaling [73] and licensing [72]. devenv, which probes for free ports so several projects can run at once [63], could fill this arm; whether it runs Postgres natively was not verified. The confound is a design inference.
- **R2 [must-fix] An off-the-shelf isolation tool, not only a baseline the Stack team wrote.** Candidates, used as documented: wt [69], Worktrunk's Postgres recipe [15][67], Coasts [24], Supabase stack mode [58]. Or have the Section 4.2 baseline written or reviewed by an independent engineer with an equal time budget. Without this, a skeptic can call the baseline sandbagged (author-allegiance bias; design inference).
- **R3 [should-fix] Ergonomics parity.** Give the baseline the same agent-facing interface: a `bin/env --json` that prints ports and URLs, documented in AGENTS.md. Otherwise the comparison partly measures "a discoverable JSON CLI" against "scripts the agent must find". Design inference.
- **R4 [should-fix] Testcontainers-only baseline for test-centric scenarios.** For scenarios 1 and 3, ephemeral databases on random host ports per test run [60] may largely solve collisions with no per-worktree service. It is likely the cheapest strong baseline there (design inference).
- **R5 [should-fix] Report the container/VM-per-agent arm as a full arm.** Options: a devcontainer per worktree, container-use [18], or a Compose stack per agent container. Vendors' cloud agents use this model [4][7][9][11].

**Evaluation and harness pitfalls**
- **R6 [must-fix] Who provisions.** If the harness runs `stack up` or `bin/setup`, the benchmark measures isolation. If the agent runs it, it measures usability and adoption. Specify which, and ideally run both strata. Design inference.
- **R7 [must-fix] User-level agent state carries over between trials and arms.** Examples: Claude Code's per-project auto-memory and user CLAUDE.md, global MCP servers, plugins and hooks, session history, `~/.codex` state, CLI auto-update. Give each trial a fresh HOME or config directory. Disable auto-memory and auto-update. Pin and record the CLI version and exact model ID. Interleave arms so silent provider-side model updates hit all arms equally. Design inference.
- **R8 [must-fix] Local agents run with host access, so they can reach the grader.** Unlike containerized benchmarks, worktree agents can read hidden validators, snapshots, canary definitions and sibling worktrees on the same filesystem. Run agents as a separate OS user, with the grader and snapshots unreadable. Keep grader code out of every repo history agents can reach. Build the fixture repo from a history that never contained `bench/agentvalue`'s grader or reference solutions; this repo's commit 05c0211 added both. Design inference, extending the git-history leaks in [80][81][120].
- **R9 [must-fix] Gaming specific to this benchmark** (extends [83][84]). Agents can satisfy "no contamination" or "clean teardown" by never starting services: mocking the DB, switching to SQLite, skipping migrations, or tearing down before verifying. Oracles must require positive evidence of real service use: seeded rows in the worktree's own DB, a job actually processed by that worktree's worker, migration ledger entries. Also detect agents bypassing or disabling Stack, for example with a host-global `docker run postgres` or `brew services`. One check: flag listeners not owned by the arm's supervisor.
- **R10 [must-fix] Arms must not share a Docker daemon, Postgres server or ports at the same time.** Run arms serially on one host, or on identical separate hosts. Reset Docker (containers, volumes, networks) and host listeners between trials. Otherwise one arm's leftovers, or a `docker system prune`, corrupts the next arm's run and its leftover scan. Take a host snapshot before each trial so the scan can attribute leftovers. Design inference.
- **R11 [must-fix] Task-selection bias.** All nine scenarios come from failure modes Stack is designed to fix, and the trap scenarios (6, 7) are built adversarially. Add control scenarios with no service dependency, to measure Stack's overhead or harm: tokens, setup time, tool-description cost [127][128]. Do not headline an aggregate across scenarios; there is no prevalence data to weight them (Section 8).
- **R12 [should-fix] Instrumentation must not create isolation.** Per-worktree Postgres roles added only for attribution would block cross-database writes in topology A and change the treatment. Attribute from server logs plus PID/cwd mapping instead of privilege boundaries. Design inference.
- **R13 [should-fix] Shared caches on both sides of the arm boundary.** Beyond mise, images and DB templates: Stack's own persistent state (template databases, supervisor state, port registry), uv, pip and npm caches, and provider-side prompt caching, which can change latency, cost and timeout behavior. Reset all of these, or stratify by them. Design inference.
- **R14 [should-fix] Start-time jitter as a factor.** Simultaneous starts stress setup races; idempotence is not enough without a lock [42]. Staggered starts would hide those races. Pre-register the start schedule and test both.
- **R15 [should-fix] Shared API rate limits.** Five or ten concurrent agents on one key can hit 429s that correlate with N and time of day. Log them, classify them as infra errors under a pre-registered rule, and keep the same key and tier across arms. Design inference.
- **R16 [should-fix] Episode-level success as a system metric.** Report "all N agents verified and zero contamination" per episode, analogous to pass^k [92][102], next to per-agent pass@1.
- **R17 [should-fix] External validity.** There is one fixture app (Python/Postgres/Redis) and apparently one agent harness. Scaffolds alone swing scores by 11–15 points [87]. Run at least two agent CLIs (for example Claude Code and Codex), and state the single-stack limitation in the claims.
- **R18 [should-fix] Host and platform confound.** macOS (Docker Desktop VM [71]) and Linux (native Docker) differ sharply for topology B. Fix and report host OS, CPU and RAM. Per-task resource limits [86] are hard to enforce for native local processes; state how limits will be applied or reported per arm.
- **R19 [should-fix] Scenario 9 is asymmetric by construction.** Topology A, a shared server, cannot run different Postgres major versions per worktree; topology B and Stack can. State this as a designed difference, not drift detection, since no drift incident was found (Section 8). Design inference.
- **R20 [should-fix] Blinding.** Graders and transcript readers should not see arm labels where possible. Publish the harness, baseline and pre-registration so the Stack team's allegiance is auditable. Design inference.

---

## 8. Open questions and evidence gaps

**Cross-cutting**
- No frequency data (surveys, share of users affected) for any failure mode. Prevalence can only be inferred from duplicate issues and upvotes, and stale-bot closures hide it.
- No prior benchmark or study evaluates per-worktree service isolation against plain worktrees with concurrent agents.
- No statistics source covers interference between agents in one episode. Episode-level clustering is our inference.

**Tools report**
- Codex CLI (as opposed to the app or cloud): worktree and service handling not researched.
- container-use `environment_add_service`: undocumented.
- Sculptor docs unreachable (DNS failure).
- Vibe Kanban: per-workspace port allocation unconfirmed.
- Copilot cloud agent: no Copilot-specific Postgres example; concurrency isolation is implied, not stated.
- Codespaces port forwarding and multiple codespaces: not opened.
- Conductor cloud workspaces: service handling not researched.
- No official claude-code issue specifically on port or DB conflicts was searched for.
- Docker Sandboxes, Ona/Gitpod, Coder and Nimbalyst not covered.
- Claude desktop: about 129k characters of the page unread, so "no database isolation" rests on a partial read.

**Failures report**
- No first-hand Redis or queue cross-talk incident; the Open Food Facts doc describes the hazard without a dated incident.
- No first-hand incident of runtime-version drift.
- No Reddit sources opened.
- codex#27584 seen only through the OpenAI forum.
- Several vendor or SEO blogs excluded; their figures are unverified.
- Not checked whether recent Claude Code releases fixed the git lock races (#47266, #55724) or orphaned processes (#43944).
- **Unverified:** a reported `pkill` self-match guard in claude-code #94469 / 2.1.214 was not opened.

**Isolation report**
- process-compose docs returned 404; Foreman and honcho not opened.
- No official devcontainer statement on worktrees or on several copies of one compose devcontainer; the `.git` pointer problem rests on community guides that were not opened.
- PlanetScale, asdf and Nix flakes not researched.
- No measured comparison of Docker Desktop and OrbStack.
- Testcontainers reuse caveats documented only for Java.
- No Postgres timing figures for template cloning; must be measured.
- Tilt's multi-instance behavior beyond `--port` is undocumented.
- No data on how many teams use compose-per-worktree versus a shared server versus devcontainers.

**Benchmarks report**
- No multi-agent, stateful-service benchmark exists.
- OpenHands eval docs not opened.
- SWE-bench Multimodal paper not opened. (Issue #465 itself was opened by the DX-evals report [120].)
- METR HCAST paper not opened.
- OpenAI's contamination red-team method truncated in the fetch.
- An un-peer-reviewed harness-disclosure preprint not cited.

**Stats report**
- No primary source for a McNemar power formula.
- Miller's exact paired and clustered SE formulas not confirmed.
- Kistin & Silverstein cited in place of Leon et al. (2011), whose full text was unavailable.
- No quantification of cache-warming or learning effects in agent environments.
- Time-of-day effect unquantified.
- No source on multi-agent SUTVA.
- METR RCT exact CI bounds not read.
- DeltaSelect not cited.
- CASRAI is a secondary source; it agrees with Cochrane.

**DX-evals report**
- No controlled with/without-browser or with/without-sandbox ablation.
- METR's 5 contributing factors were in an image and could not be read.
- OpenAI trace-grading docs not opened.
- CAID (worktree-per-agent) and the AIDev merge-conflict study seen only in snippets and not cited.
- Terminal-Bench reset/cleanup text not read.
- LSP token-savings practitioner claims left out as uncontrolled.
- ESEM paper read via pdftotext.

**Questions the benchmark itself has to answer**
- Template-clone and container-start latency per arm.
- RAM per agent at N=5 under each topology.
- Whether agents in arm 2 adopt Stack unprompted.
- Whether the competent baseline's lock-guarded setup holds at N=5 and N=10.

---

## 9. Verification ledger

| Topic | Claims | Supported | Partially supported (corrected wording used) | Unsupported | Unreachable |
|---|---|---|---|---|---|
| tools | 26 | 18 | 8 (c6, c11, c12, c19, c20, c22, c24, c25) | 0 | 0 |
| failures | 26 | 19 | 7 (c7, c8, c10, c11, c22, c23, c26) | 0 | 0 |
| isolation | 36 | 27 | 9 (c10, c14, c17, c18, c20, c23, c24, c29, c32) | 0 | 0 |
| benchmarks | 27 | 20 | 7 (C6, C7, C8, C10, C19, C22, C27) | 0 | 0 |
| stats | 25 | 21 | 4 (c1, c16, c18, c20) | 0 | 0 |
| dxevals | 26 | 22 | 4 (c4, c12, c17, c25) | 0 | 0 |
| **Total** | **166** | **127** | **39** | **0** | **0** |

**No claim was rated unsupported, so no claim was dropped whole.** These sub-claims inside partially supported claims were removed or corrected:
- tools c6: "Codex worktrees copy only repo files" became "own repo files, dependencies and build caches".
- tools c11: "only the active tab gets a localhost mapping" became "the active tab gets the preferred mapping; others fall back to random ports".
- tools c12: "Cursor offers no built-in DB isolation" is not stated in the thread.
- tools c19: container-use labelled "experimental"; not found.
- tools c20: Sculptor's "single local dev environment" is inference.
- tools c22: "Codespaces runs each codespace as a container on a VM / each environment brings its own full service stack" is not on the cited page.
- tools c24: attribution to "the Aspire team" became a guest post by Tamir Dresher; the MCP proxy was added.
- tools c25: the quote "isolated dbs for integration tests, shared services for UI" was not found in the thread.
- failures c7: "(data loss / destructive foot-gun)" label is not on the issue; only P1-high.
- failures c8: the sibling kill is "consistent with", not certain; lsof was used for evidence, not to detect the kill.
- failures c10: a single incident, not a pattern; closed for inactivity.
- failures c11: "users rely on third-party plugins" became "one commenter's plugin".
- failures c22: "tools not activated in agent shells" is not on the page.
- failures c23: attribution to Eric Allam / Trigger.dev founder is not on the page.
- failures c26: irskep's steps are "ergonomic annoyances he automated", not what keeps him from using worktrees.
- isolation c10: the "per-feature" intent for Neon branches is not stated.
- isolation c14: "most requested" applies to hand-coordinating IDs and ports, not to per-agent projects.
- isolation c17: per-directory pinning and `[env]` are not on the mise.lock page; `mise.local.toml` is not described as "personal overrides".
- isolation c18: devenv running Postgres natively and `$DEVENV_STATE` are not stated.
- isolation c20: Overmind multi-instance guidance is inference.
- isolation c23: "typically symlinks .env" is overstated, and the non-concurrent mode is not on the env-vars page (it is on [12]).
- isolation c24: the per-workspace DB is a test DB.
- isolation c29: wt's Redis is optional and the registry is at the repo root, not machine-local.
- isolation c32: "free only" is too absolute.
- benchmarks C6: 6.2 points corrected to 6.4.
- benchmarks C7: reflogs leaking is not on bayes.net; the issue number is not on that page.
- benchmarks C8: PR #581 was closed unmerged; inflation comes from the author's own audit.
- benchmarks C10: the link between 43x and the visible scoring function is hedged by METR.
- benchmarks C19: the commercial set is proprietary startup code, not GPL; the human check covers all tests.
- benchmarks C22: about 50% single-trial corrected to about 61% retail pass^1.
- benchmarks C27: 5–50% was a design goal; o1 scored 61.7%.
- stats c1: "cluster on unit of randomization" is from Anthropic's post [98], not Miller's paper.
- stats c16: "heavy-tailed" is our inference.
- stats c18: "no clear learning effect in 30–50 h" is in the arXiv paper [106], not the blog.
- stats c20: the paired t-test applies only when carryover and period effects are not a problem.
- dxevals c4: "found answers in git history" became "gained an unfair advantage from git history".
- dxevals c12: the LSP study ran arms A–D only; "ignored" holds for localization only; the abstract's token-saving finding is in tension with the paper's own Section 6.5; tool-description length matching is not in the corrected claim.
- dxevals c17: reflog and branch leakage listed as vectors, not shown in trajectories; about 5 in 10,000 trajectories affected.
- dxevals c25: the MCP-Universe attribution is unverified.

**Critique corrections applied on top of the verifier.** The critic checked the draft against the claim text. It removed invented quotes, figures not in any claim, and over-broad wording; fixed internal inconsistencies; and added the uncited Neon and Supabase branching references to Section 4.1. Uncited references from the source reports (a Cursor forum thread, the Aider polyglot post, and Anthropic's "Building effective agents") were dropped from the list.

In a few cases the verifier's note on the same claim appears to support a detail the critic had removed or softened. These details are left out here. Restore them only after re-checking the source:
- the abort-option effect sizes and the comparison across models (benchmarks C12);
- the `hash_port` port range (tools c16);
- the Cursor cloud page's exact machine and startup-command wording (tools c10);
- the self-hosted scope of the single-use-runner advice (tools c13);
- Conductor's workspace-name variable (tools c14);
- the example container name in the hop-top PR (isolation c3);
- the exact Compose project-name character rules (isolation c2);
- the ESEM study's "preliminary" label and sample size (dxevals c19);
- the agent scope of the SWE-MISBEHAVIOR figures (dxevals c23).

For CORE-Bench (stats c12), the verifier says the 42%→95% jump came from more than grader fixes. The text therefore says "among other changes".

---

## References

1. https://code.claude.com/docs/en/worktrees.md (also opened as /docs/en/worktrees): Claude Code docs, Run parallel sessions with worktrees
2. https://code.claude.com/docs/en/agents.md: Claude Code docs, Run agents in parallel
3. https://code.claude.com/docs/en/desktop.md: Claude Code docs, Desktop app
4. https://code.claude.com/docs/en/cloud-environments.md: Claude Code docs, Configure cloud environments
5. https://learn.chatgpt.com/docs/environments/git-worktrees: Codex docs, Git worktrees
6. https://learn.chatgpt.com/docs/environments/local-environment.md (also without .md): Codex docs, Local environments
7. https://learn.chatgpt.com/docs/environments/cloud-environment: Codex docs, Cloud environment
8. https://cursor.com/docs/configuration/worktrees: Cursor docs, Worktrees
9. https://cursor.com/docs/cloud-agent/setup: Cursor docs, Cloud Environment Setup
10. https://forum.cursor.com/t/docs-guidance-on-cloud-agent-port-forwarding/156960: Cursor forum, Docs/guidance on Cloud Agent port forwarding
11. https://docs.github.com/en/copilot/how-tos/use-copilot-agents/coding-agent/customize-the-agent-environment: GitHub Docs, Customize the agent environment
12. https://www.conductor.build/docs/reference/scripts: Conductor docs, Scripts reference
13. https://www.conductor.build/docs/reference/environment-variables: Conductor docs, Environment variables
14. https://vibekanban.com/docs/settings/projects-repositories: Vibe Kanban docs, Projects & repositories
15. https://worktrunk.dev/tips-patterns/: Worktrunk, Tips & patterns
16. https://github.com/devflowinc/uzi: uzi README
17. https://github.com/d-kuro/gwq: gwq README
18. https://container-use.com/llms-full.txt: container-use docs
19. https://imbue.com/sculptor/: Imbue, Sculptor product page
20. https://github.com/stravu/crystal: stravu/crystal README
21. https://containers.dev/guide/dockerfile: containers.dev, Using Images, Dockerfiles, and Docker Compose
22. https://dev.to/augusto_chirico/claude-code-loves-worktrees-your-infrastructure-doesnt-kfi: DEV, Claude Code loves worktrees, your infrastructure doesn't (anecdote)
23. https://devblogs.microsoft.com/aspire/scaling-ai-agents-with-aspire-isolation/ (also https://devblogs.microsoft.com/aspire/?p=238): Aspire blog, Scaling AI agents with Aspire isolation (guest post)
24. https://news.ycombinator.com/item?id=47575417: Show HN, Coasts (anecdotes)
25. https://engineering.getdexter.co/2025/08/31/parallel-rails-dev-environments: Dexter Engineering, Running Multiple Rails Environments in Parallel
26. https://openfoodfacts.github.io/openfoodfacts-server/dev/how-to-run-several-worktrees/: Open Food Facts, How to run several worktrees (or several agents) on one machine
27. https://github.com/k2b-dev/cloud/issues/843: k2b-dev/cloud #843
28. https://github.com/jedbjorn/subfloor/issues/569: jedbjorn/subfloor #569
29. https://github.com/anthropics/claude-code/issues/90070: claude-code #90070, Unscoped pkill killed a sibling project's dev server
30. https://github.com/anthropics/claude-code/issues/104730: claude-code #104730
31. https://github.com/anthropics/claude-code/issues/5070: claude-code #5070
32. https://github.com/anthropics/claude-code/issues/43944: claude-code #43944
33. https://github.com/anthropics/claude-code/issues/16198: claude-code #16198
34. https://github.com/anthropics/claude-code/issues/27744: claude-code #27744, Add PostWorktreeCreate hook for environment initialization
35. https://community.openai.com/t/codex-worktrees-need-a-runtime-preflight-for-missing-env-files/1393774: OpenAI Developer Community, Codex worktrees need a runtime preflight (anecdote)
36. https://github.com/anthropics/claude-code/issues/41075: claude-code #41075
37. https://forum.cursor.com/t/windows-request-to-disable-automatic-worktree-creation-critical-disk-space-issue/146189: Cursor forum, disk space issue (anecdote)
38. https://github.com/anthropics/claude-code/issues/47266: claude-code #47266
39. https://github.com/anthropics/claude-code/issues/55724: claude-code #55724
40. https://johnnunemaker.com/conducting-rails/: John Nunemaker, Conducting Rails
41. https://dgalarza-github-io.pages.dev/posts/2026-03-10-extending-claude-code-worktrees-for-true-database-isolation: Damian Galarza, Extending Claude Code Worktrees for True Database Isolation
42. https://piechowski.io/post/setup-script-git-worktrees/: Ally Piechowski, Your Setup Script Should Support Git Worktrees
43. https://dev.to/maverickdotdev/we-ditched-worktrees-for-claude-code-heres-what-we-use-instead-5b22: DEV, We ditched worktrees for Claude Code
44. https://newreleases.io/project/github/jdx/mise/release/v2026.7.5: mise v2026.7.5 release notes
45. https://github.com/Myeongseok-Kang/claude-bulkhead: claude-bulkhead README
46. https://news.ycombinator.com/item?id=49413093: HN, Parallel development without the headaches using Git worktree (anecdotes)
47. https://docs.docker.com/compose/how-tos/project-name/ (also .md): Docker docs, Specify a project name
48. https://github.com/hop-top/git/pull/16: hop-top/git PR #16
49. https://docs.docker.com/reference/compose-file/services/: Docker docs, Compose services reference
50. https://docs.docker.com/compose/how-tos/environment-variables/variable-interpolation/: Docker docs, Variable interpolation
51. https://docs.docker.com/compose/how-tos/profiles/: Docker docs, Profiles
52. https://docs.docker.com/reference/cli/docker/compose/down/: Docker docs, docker compose down
53. https://www.postgresql.org/docs/current/manage-ag-templatedbs.html: PostgreSQL docs, Template Databases
54. https://www.postgresql.org/docs/current/sql-createdatabase.html: PostgreSQL docs, CREATE DATABASE
55. https://neon.com/docs/introduction/branching: Neon docs, Branching
56. https://neon.com/pricing: Neon pricing
57. https://supabase.com/docs/guides/deployment/branching: Supabase docs, Branching
58. https://supabase.com/docs/guides/local-development/running-multiple-local-projects: Supabase docs, Running multiple local projects
59. https://supabase.com/blog/local-supabase-project-per-agent-with-or-without-docker: Supabase blog, Local Supabase project per agent
60. https://java.testcontainers.org/features/networking/: Testcontainers, Networking
61. https://java.testcontainers.org/features/configuration/: Testcontainers, Configuration
62. https://mise.jdx.dev/dev-tools/mise-lock.html: mise docs, mise.lock
63. https://devenv.sh/processes/: devenv, Processes
64. https://direnv.net/: direnv
65. https://github.com/DarthSim/overmind: Overmind README
66. https://docs.tilt.dev/cli/tilt_up.html: Tilt docs, tilt up
67. https://madewithlove.com/blog/worktrunk-takes-the-friction-out-of-git-worktrees.md: madewithlove, Worktrunk takes the friction out of git worktrees
68. https://forum.elixirforum.com/t/elixir-worktree-skill-a-small-claude-code-skill-for-parallel-git-worktree-phoenix-sessions-port-postgres-isolation/75662/2: Elixir Forum, elixir-worktree skill (anecdote)
69. https://github.com/tokenbooks/wt: tokenbooks/wt README
70. https://redis.io/docs/latest/commands/select/: Redis docs, SELECT
71. https://docs.docker.com/desktop/settings-and-maintenance/settings/: Docker Desktop settings
72. https://www.docker.com/pricing/faq/: Docker pricing FAQ
73. https://docs.docker.com/desktop/features/synchronized-file-sharing/: Docker docs, Synchronized file shares
74. https://code.visualstudio.com/docs/devcontainers/create-dev-container: VS Code docs, Create a Dev Container
75. https://openai.com/index/why-we-no-longer-evaluate-swe-bench-verified/: OpenAI, Why SWE-bench Verified no longer measures frontier coding capabilities
76. https://openai.com/index/introducing-swe-bench-verified/: OpenAI, Introducing SWE-bench Verified
77. https://arxiv.org/abs/2506.09289: UTBoost
78. https://arxiv.org/abs/2410.06992: SWE-Bench+
79. https://arxiv.org/abs/2503.15223: Are "Solved Issues" in SWE-bench Really Solved Correctly? (PatchDiff)
80. https://bayes.net/swebench-hack/: Claude 4 hacked SWE-bench by peeking at future commits
81. https://github.com/SWE-bench/SWE-bench/pull/581: SWE-bench PR #581
82. https://www.poolside.ai/blog/through-the-looking-glass: Poolside, Through the looking glass of benchmark hacking
83. https://metr.org/blog/2025-06-05-recent-reward-hacking/: METR, Recent Frontier Models Are Reward Hacking
84. https://arxiv.org/abs/2510.20270: ImpossibleBench
85. https://arxiv.org/html/2601.11868v1 (also /html/2601.11868): Terminal-Bench
86. https://www.anthropic.com/research/infrastructure-noise (also /engineering/infrastructure-noise): Anthropic, Quantifying infrastructure noise in agentic coding evals
87. https://epochai.substack.com/p/why-benchmarking-is-hard: Epoch AI, Why benchmarking is hard
88. https://epoch.ai/benchmarks/swe-bench-verified: Epoch AI, SWE-bench Verified
89. https://arxiv.org/abs/2509.16941: SWE-Bench Pro
90. https://arxiv.org/abs/2505.20411: SWE-rebench
91. https://arxiv.org/abs/2504.02605: Multi-SWE-bench
92. https://arxiv.org/abs/2406.12045 (also https://arxiv.org/pdf/2406.12045): tau-bench
93. https://arxiv.org/abs/2507.02825: Establishing Best Practices for Building Rigorous Agentic Benchmarks (ABC)
94. https://arxiv.org/abs/2502.12115: SWE-Lancer
95. https://metr.org/blog/2025-08-12-research-update-towards-reconciling-slowdown-with-time-horizons/: METR, Algorithmic vs. Holistic Evaluation
96. https://arxiv.org/abs/2503.14499 (also /html/2503.14499v3): METR, Measuring AI Ability to Complete Long Software Tasks
97. https://arxiv.org/html/2411.00640: Miller, Adding Error Bars to Evals
98. https://www.anthropic.com/research/statistical-approach-to-model-evals: Anthropic, A statistical approach to model evaluations
99. https://arxiv.org/html/2602.07150v2: On Randomness in Agentic Evals
100. https://thinkingmachines.ai/blog/defeating-nondeterminism-in-llm-inference/: Thinking Machines, Defeating Nondeterminism in LLM Inference
101. https://arxiv.org/html/2308.02828v2: Ouyang et al., Non-determinism of ChatGPT in Code Generation
102. https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents: Anthropic, Demystifying evals for AI agents
103. https://aclanthology.org/P18-1128.pdf: Dror et al., The Hitchhiker's Guide to Testing Statistical Significance in NLP
104. https://arxiv.org/abs/2503.01747: Bowyer et al., Don't Use the CLT in LLM Evals With Fewer Than a Few Hundred Datapoints
105. https://arxiv.org/abs/2604.22750: Bai et al., How Do AI Agents Spend Your Money?
106. https://arxiv.org/html/2507.09089: METR, Measuring the Impact of Early-2025 AI on Experienced OS Developer Productivity
107. https://metr.org/blog/2025-07-10-early-2025-ai-experienced-os-dev-study/: METR blog, early-2025 developer study
108. https://www.cochrane.org/authors/handbooks-and-manuals/handbook/current/chapter-23: Cochrane Handbook, Chapter 23
109. https://www.casrai.org/guides/crossover-trial-design-and-carryover: CASRAI, Crossover trial design and carryover
110. https://pmc.ncbi.nlm.nih.gov/articles/PMC4917389: Kistin & Silverstein, Pilot Studies (JAMA 2015)
111. https://neurips.cc/virtual/2021/workshop/21885: NeurIPS 2021 Workshop, Pre-registration in Machine Learning
112. https://arxiv.org/html/2407.01502: Kapoor et al., AI Agents That Matter
113. https://arxiv.org/abs/2609.33812: Identical Runs, Different Results
114. https://arxiv.org/html/2405.15793: SWE-agent
115. https://github.com/SWE-agent/mini-swe-agent: mini-swe-agent README
116. https://www.anthropic.com/engineering/writing-tools-for-agents: Anthropic, Writing effective tools for agents
117. https://metr.org/blog/2026-02-24-uplift-update/: METR, uplift update (Feb 2026)
118. https://arxiv.org/html/2608.13568: Does a Language Server Save Tokens for Coding Agents?
119. https://arxiv.org/html/2509.24002: MCPMark
120. https://github.com/SWE-bench/SWE-bench/issues/465: SWE-bench issue #465, Repo State Loopholes During Agentic Evaluation
121. https://developers.openai.com/blog/eval-skills: OpenAI, Testing Agent Skills Systematically with Evals
122. https://drops.dagstuhl.de/storage/00lipics/lipics-vol394-esem2026/LIPIcs.ESEM.2026.18/LIPIcs.ESEM.2026.18.pdf: Özaytürk & Buzluca, Coordinating Agents on a Shared Git Workspace (ESEM 2026)
123. https://www.arxiv.org/abs/2601.13295: CooperBench
124. https://arxiv.org/abs/2507.09063: SetupBench
125. https://arxiv.org/abs/2606.01317: SABER
126. https://arxiv.org/abs/2610.18206: SWE-MISBEHAVIOR
127. https://arxiv.org/abs/2602.11988: Evaluating AGENTS.md
128. https://arxiv.org/abs/2602.14878: MCP Tool Descriptions Are Smelly!
129. https://www.anthropic.com/research/measuring-agent-autonomy: Anthropic, Measuring AI agent autonomy in practice
