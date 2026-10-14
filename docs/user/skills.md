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

Stack asks mise only about the releases `stack.lock` pins, so the answer is the same in a
fresh worktree. Nothing is installed or written into the project.

Without `stack.lock` nothing is pinned and every entry is `unavailable`. When mise cannot answer
(not installed, older than 2026.9.2, a timeout or unreadable output), every entry is
`unavailable`, `warnings` carries `skills_unavailable`, and the command still succeeds.

### The provider's skills

Stack adds Pitchfork to run services. Pitchfork's skill teaches an agent to start and stop
daemons itself, around stack's ownership, identity checks and leases, so it is never listed
in `skills`, never returned by `stack_skill`, and never linked. People can see it with
`stack inspect --all-skills` (MCP `stack_inspect` with `all_skills: true`), under
`provider_skills`.

## Reading a skill over MCP

`stack_skill { tool, name }` returns `{ tool, version, name, entrypoint, bytes, text }` for one
`available` skill from `skills`. `name` may be left out when the tool has exactly one available
skill; with several, the `usage` error lists their names. The MCP server's instructions
mention it, so agents find it without being told.

Stack returns the text as the release ships it, without edits. fnox 1.39.0's skill, for
example, is one `SKILL.md` whose provider details are links to <https://fnox.jdx.dev>; the
release carries no provider pages, so an agent without network access cannot follow them.

| Error | When |
|---|---|
| `skill_not_found` | not listed as `available` in `skills`: unknown tool or name, a provider tool, a release that is not installed or ships no such skill, or a listing that was unavailable |
| `skill_unreadable` | the entrypoint is no longer a regular file inside the release (checked again when read, links not followed), cannot be read, or is not UTF-8 |
| `skill_too_large` | `SKILL.md` is larger than 64 KiB; read the `entrypoint` file directly |

## Linking skills into the project

To have `stack up` and `stack install` link skills where your agent looks for them:

```toml
# stack.toml (project only; a bundle cannot set it)
[skills]
dir = ".claude/skills"
```

Each `available` skill is linked to `<dir>/<name>`, and the links are recorded in
`<dir>/.stack-skills.json`.

- The links point into your own mise installs, so in a Git checkout Stack lists them in a
  `.gitignore` of its own in that directory. Skills you add there yourself stay visible.
- Stack only replaces or removes a link it made that still points where it left it. Anything
  else at that name is left alone and reported under `kept`.
- If two releases ship a skill with the same name, neither is linked (`skipped`).
- Links to skills that are no longer available are removed (`pruned`).
- `dir` must be a relative path inside the project, outside `.stack` and `.git`, with no
  symbolic link along the way (`invalid_path`).
- Problems in this step are warnings. They never fail `up` or `install`.

Stack links skills itself rather than running `mise skills sync`, which would also link
Pitchfork's skill.

[All docs](../README.md)
