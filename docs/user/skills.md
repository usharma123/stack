# Agent skills

Some tools ship an agent skill: a directory with a `SKILL.md` describing how to use the tool,
installed with the tool by mise's packslip backend (fnox, Mr Boxington and Pitchfork do). Stack
lists the skills of exactly the releases `stack.lock` pins, returns one over MCP, and can link
them into a directory agents read. It never runs or interprets a skill; a skill is the tool's
own documentation at that release.

## Listing skills

`stack inspect --json`, `stack compile --json` and MCP `stack_inspect` report `skills`, one
entry per pinned tool and preset service tool, and one per skill when a release ships several:

```json
"skills": [
  { "tool": "fnox", "version": "1.39.0", "origin": "project", "status": "available", "name": "fnox",
    "directory": "~/.local/share/mise/installs/fnox/1.39.0/.mise-packslip/repo/skills/fnox",
    "entrypoint": "~/.local/share/mise/installs/fnox/1.39.0/.mise-packslip/repo/skills/fnox/SKILL.md" },
  { "tool": "jq", "version": "1.7.1", "origin": "project", "status": "no_skill" },
  { "tool": "postgres", "version": "17.11", "origin": "project", "services": ["db"], "status": "not_installed",
    "reason": "the pinned release is not installed; run `stack install`" }
]
```

(Paths are absolute in real output.)

| Status | Meaning |
|---|---|
| `available` | the pinned release is installed and ships this skill |
| `no_skill` | the pinned release is installed and ships no usable skill (`reason` says why when one was listed but refused) |
| `not_installed` | the pinned release is not installed on this machine; `stack install` installs it |
| `unavailable` | stack could not tell: nothing pinned yet, or mise could not answer |

How it works: stack writes a configuration naming every pin at its exact release (services as
their preset's tool, with the tool's allowlisted options) and nothing else into a scratch
directory of its own under stack's cache, runs `mise ls --json` and `mise skills ls --json`
there (10 seconds each, at the same time), and removes the directory. Rows for tools or
releases stack.lock does not pin are ignored, so the answer is the same in a fresh worktree
without a generated config and in one whose generated config is stale. No `[env]` template or
task of the project's is evaluated, nothing is installed, and nothing is written into the
project. mise does record each scratch configuration among its tracked configs (entries that
point at removed files).

A skill is `available` only when its name matches `[a-z0-9][a-z0-9_-]*` and its directory and
`SKILL.md`, with links resolved, lie inside the install directory mise reports for the pinned
release. Anything else is `no_skill` with a `reason`.

Without `stack.lock` (before the first `compile`) nothing is pinned: every entry is
`unavailable` and mise is not asked, rather than reporting whatever release happens to be
active. When mise cannot answer (not installed, older than 2026.9.2, which added `mise
skills`, a timeout, or output stack cannot read), every entry is `unavailable`, `warnings`
carries `skills_unavailable: ...`, and the command still succeeds.

### The provider's skills

Stack adds Pitchfork to run services. Pitchfork's skill teaches an agent to start and stop
daemons itself, around stack's ownership, identity checks and leases, so it is never listed
in `skills`, never returned by `stack_skill`, and never linked. People can see it with
`stack inspect --all-skills` (MCP `stack_inspect` with `all_skills: true`), under
`provider_skills`.

## Reading a skill over MCP

`stack_skill { tool, name }` returns `{ tool, version, name, entrypoint, bytes, text }` for one
`available` skill from `skills`. The MCP server's instructions mention it, so agents find it
without being told.

| Error | When |
|---|---|
| `skill_not_found` | not listed as `available` in `skills`: unknown tool or name, a provider tool, a release that is not installed or ships no such skill, or a listing that was unavailable |
| `skill_unreadable` | the entrypoint is no longer a regular file inside the release (checked again when read, links not followed), cannot be read, or is not UTF-8 |
| `skill_too_large` | `SKILL.md` is larger than 64 KiB; read the `entrypoint` file directly |

## Linking skills into the project

To have `stack up` and `stack install` link skills where an agent looks for them:

```toml
# stack.toml
[skills]
dir = ".claude/skills"
```

and keep the links out of version control; they point into one user's mise installs:

```gitignore
.claude/skills/
```

- Project only: a bundle cannot set `[skills]` (`bundle_invalid`).
- `dir` must be relative, stay inside the project and not be inside `.stack` or `.git`
  (`invalid_path` at compile). When linking, every existing component of the path must be a
  real directory: a symbolic link anywhere in it is refused (`invalid_path`), so links cannot
  be written outside the project.
- After the install step, a `skills` step links every `available` skill to `<dir>/<name>` and
  records what it linked in `<dir>/.stack-skills.json`. Stack replaces or removes only a
  symbolic link whose current target is the one it recorded. A real directory, a file, a link
  stack did not make, or one of stack's links pointed elsewhere since is left alone, reported
  under `kept` with a reason, and no longer considered stack's.
- Two pinned releases shipping a skill of the same name: neither is linked; the step lists it
  under `skipped`.
- Links to skills no longer available (a tool removed, a release changed) are removed if they
  are still stack's (`pruned`). A directory with nothing to link is not created.
- A `.stack-skills.json` that is a link, not a regular file, malformed, of an unknown version,
  or naming something other than a plain skill name makes stack change nothing in the
  directory; move it aside to start afresh.
- Nothing in this step fails `up` or `install`. Problems make the step's status `warning`, are
  listed in its `detail.warnings` (`{ code, message }` with code `skills_unavailable`,
  `invalid_path` or `skills_failed`) and in the result's top-level `warnings`, and are printed
  to stderr without `--json`.
- Stack links skills itself rather than running `mise skills sync`, which would also link the
  provider's skill.

[All docs](../README.md)
