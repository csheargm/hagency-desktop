# 0022. "Push freely" covers the fix steps of an issue

Status: accepted (Chris, 2026-11-17)

## Context
With issue watching on and "Push and open PRs freely" on, Hagency answered
the push, `gh pr create|ready|edit`, `fetch`, `checkout -b <new>
<remote>/<default>` and workspace file edits without a card (ADR 0015), and
read-only `gh` queries (ADR 0019). Everything between, the ordinary local
steps of a fix, still asked. Live (2026-11-17): bird-claude, watch mode "all"
on csheargm/hagency-demo-sandbox with push freely on, lost its run when the
card for its first shell step (a `git fetch` plus a read of the README) went
unanswered and expired. An autonomous issue loop cannot wait for a person at
every `git status`.

## Decision
When ALL hold, these calls run without a card:
1. The call comes from a run in the watch's report Room requested by the
   owner (the issue-watch hand-off is the owner's notice): a foreground
   Claude call, or a Codex call of the host tool `watched_repo_step` or a
   Codex file change (see "Codex" below).
2. The watch is active (a paused legacy watch grants nothing; a work-list
   entry replaces a legacy watch of the same repository) and push freely is
   on. Both are re-read for every call.
3. The target is the watched repository's OWN checkout: the top of a plain
   clone inside the workspace (real `.git` directory, no `commondir`, no
   `.git` file), resolved through every link (no `..` or symlink escape),
   whose local config holds data keys only (no `alias.*`, `core.*` programs,
   `credential.*`, `include.*`, filters, `core.worktree`...), with no
   `.gitmodules` and no `info/attributes`, and whose EVERY remote fetches
   from and pushes to exactly that github.com repository.
4. The call is one command (no `;`, `&&`, `|`, `$(...)`, redirects, env
   prefix; a heredoc only as the message of `commit -F -`) from this list:
   - `git -C <checkout>` (or bare `git` when the turn's directory is the
     checkout) `status`, `diff`, `log`, `show`, `branch` (listing only),
     `add`, `commit -m|-F -`, `restore`, options from closed lists, every
     other word resolving inside the checkout (so `diff` cannot fall back
     to `--no-index` on outside files). These run as the Agent, rewritten to
     a pinned command: `env -u GIT_DIR ...` (14 relocation variables),
     `GIT_ATTR_SOURCE=<empty tree>` (no worktree `.gitattributes`, so no
     filter or textconv driver), `-c core.hooksPath=/dev/null`, fsmonitor,
     pager, editor, gpg signing, every transport (`protocol.allow=never`),
     submodule recursion and auto gc off, plus `--no-ext-diff
     --no-textconv` for diff/log/show and `--no-verify` for commit.
   - `fetch` (unchanged), `pull [--ff-only] [<remote> [<branch>]]`: the host
     fetches as in ADR 0015, then the Agent's call fast-forwards only, pinned.
   - `git clone <github url of the repo> [<dir>]` / `gh repo clone <repo>
     [<dir>]` into a new folder inside the workspace: the host fetches every
     branch into a fresh scratch repository (no alternates, no checkout
     config), sets its HEAD to the remote's advertised default branch; the
     Agent's call clones from that path (`protocol.file` only, empty
     template, hooks and attributes off) and points `origin` at GitHub.
   - Claude `Read`/`Glob`/`Grep`, and `Write`/`Edit`/`MultiEdit` (never git
     metadata), of a path inside that checkout. Edits elsewhere in the
     workspace now ask (narrower than ADR 0015).
   - push and `gh pr` as before. The merge gate is unchanged.

Everything else still asks: other hosts, package installs, build and test
scripts (`cargo test`, `npm test`, `make`), writes outside the checkout,
`git config`, `reset`, `rebase`, `merge`, `stash`, branch deletion, chained
shell. The settings label reads "Fix, push and open PRs freely" with a hint
saying what it covers (en, zh-CN), and the Agent's own watch block lists the
card-free shapes, so it issues them one per call.

## Threat model
- Prompt injection from issue text can now make the Agent read and edit
  files in that checkout, commit, and push a non-default branch to the
  watched repository, and open a pull request. It cannot merge (merge
  policy and its cards are untouched), push the default branch, force-push,
  delete or tag (ADR 0015), or reach another repository or host.
- Repository content cannot run code through these steps: hooks are off,
  attributes come from the empty tree, local config must be data-only and
  is pinned again at run time, and network steps never read checkout config
  (host-run). A cloned repository's `.gitattributes`, hooks directory or
  `.gitmodules` are inert or make the checkout ask.
- The owner's own global git config (`~/.gitconfig`) is still read by the
  Agent-run local steps; it is the owner's, not repository data (residual,
  as in ADR 0015). The checks and the run are not atomic against a
  concurrent writer, which itself needs an approved command.
- Inside this exact scope push freely overrides a Deny high-risk policy
  (see "Deny" below); outside it a Deny is still a Deny.

## GPT review (2026-11-17)
Adopted: `commit` takes exactly one message source and `-F` only as `-`
(git reads the last `-F`, so `-m x -F /etc/passwd` would have put any file
into a pushed commit); a clone never lands inside git metadata, named or
resolved; scratch repositories are created owner-only (0700); a pull
fast-forwards to the exact commit the host just fetched and fails when the
branch no longer exists (no stale tracking ref).
Rejected: "existing push-freely opt-ins silently expand". The widening of
this exact switch is the owner's decision (Chris, 2026-11-17); the label and
hint now say what it covers. Residual: a scratch kept for the Agent's call
is removed by that call, so a call killed in between leaves it in the
host's temp folder (owner-only).

Second pass, adopted: push trust is checked against the Room workspace,
never the proposal directory the permission callback derives from the call's
own path argument (an absolute file's parent; before this, an absolute Edit
anywhere was "inside the workspace" for ADR 0015's edit rule too); direct
reads of `.git` ask; clone, pull and fetch recheck the watch before the
network step and before handing anything to the Agent. Rejected: bounding
Glob/Grep traversal into nested checkouts: Claude already reads inside its
working directory without asking, so this adds no exposure.

## Open questions
- Should build/test scripts (`cargo test`, `npm test`) be card-free? Default
  NO: they run repository code (build scripts, test fixtures, npm lifecycle
  scripts) with the Agent's rights and token in reach; prompt injection
  plus a malicious PR branch would be code execution on the owner's
  machine. A sandboxed test runner would be the way to allow them.
- Hosted Agents default to a Deny high-risk policy (nobody sees cards).
  Answered by "Deny" below: push freely lifts it for its own scope only.

## Deny (Chris, 2026-11-17)
Live: the issue #33 hand-off run of a hosted Agent failed with "policy
declines (high-risk tools)": its default high-risk rule is Deny, and push
freely only narrowed what Ask-owner asks. Decision: inside the exact scope
above (the owner's run in the watch's report Room, the watch active, push
freely on, the steps `pinned` answers, and the owner's read-only `gh`
queries on a push-freely repository) push freely overrides the high-risk
tool rule, Deny and tool/folder rules included. `Ledger::trusted_publish`
then checks only the dispatch (binding, expiry, admitted scope, unchanged
policy revisions, requests not refused) and authorizes the step. Outside
the scope nothing changes and Deny still refuses: another repository or
checkout, a chained or unlisted command, a paused watch or push freely off,
another Room or requester (including another member's read-only query),
a saved policy change since the run was admitted, or requests refused
outright. Threat: anyone who can make the owner's own hand-off run read
injected text gets the push-freely capabilities above even where the
owner's tool rule said Deny; the switch is the owner's explicit opt-in for
that repository, and it is the only thing that grants this.

## Codex (Chris, 2026-11-17)
Why it was Claude-only: Claude's permission callback answers with
`updatedInput`, so a trusted call runs the PINNED command (relocation
variables unset, hooks, fsmonitor, pager, editor, signing, transports and
attributes off), and network steps are run by the host while the call only
prints the result. Codex's app-server approval (`item/commandExecution/
requestApproval`) answers only accept or decline for the command the model
wrote (`/bin/zsh -lc '<script>'`), which then runs in Codex's
workspace-write sandbox with no network: unpinned (the checkout's hooks and
`.gitattributes` with the owner's global drivers apply, the
`GIT_CONFIG_KEY_*` pins cannot be passed because Codex strips `*KEY*`
variables, and zsh would expand the script itself), and unable to fetch,
push, clone or call `gh` at all. A network approval would let Codex's
own git reach GitHub with the checkout's config, not the host. So the
guarantees "pinned, no git config or hooks" and "host-side network" cannot
be held for Codex's native shell, and it is still never trusted.

Mapping that holds the same guarantees:
- Commands go through the host tool `watched_repo_step` on the Room MCP
  server (`{"command": "<one line>"}`, auto-approved as Hagency's own
  tool). The turn that owns the call builds a proposal `codex.repo_step`
  rooted at the Room workspace and checks it with exactly the Claude `Bash`
  rules (`pinned`, the read-only queries, the Deny override). The HOST then
  runs the pinned command, or the host step and then its pinned follow-up,
  under `/bin/sh` in the workspace with only the host's small environment,
  the Agent's token and an empty gh config, at most 60 s, killed when the
  turn stops. The model's text is never run: only the pinned line. Refused
  calls return `not_card_free` and run nothing; the Agent then uses its own
  shell, which asks (or is refused under Deny). Always refused in a shared
  project Room (no native commands there).
- Codex file changes (`apply_patch`, `item/fileChange/requestApproval`) are
  accepted natively when every path, and every rename target, is absolute,
  inside the own checkout, never git metadata (named or resolved through
  links), with no grant root: Claude's `Edit`/`Write` rule. A deletion, a
  relative path or any unknown kind asks. Checked against the Room
  workspace, never the request's `cwd`.
- The Agent's own watch block tells a Codex Agent to send the listed steps
  through `watched_repo_step`; the settings hint names Claude and Codex and
  the Deny override (en, zh-CN).

Residual (as for Claude): the checks and the run are not atomic against a
concurrent writer; the host-run local steps read the owner's global git
config (`~/.gitconfig`). New: the host-run steps run outside Codex's
sandbox, as Claude's do; only the pinned commands run there.

### GPT review of "Deny" and "Codex" (2026-11-17)
Adopted: the override also ends when any policy layer was saved after the
run was admitted (the runtime builds each proposal with the current
revisions, so the revision check alone never caught it); a queued
`watched_repo_step` whose call was given up (its native turn ended, or the
MCP wait expired) never starts, and a running one is stopped when its call
is given up; at most two steps of a turn run at once; a host command keeps
at most 1 MiB of each output stream; a trusted Codex file change is checked
again right before its accept is sent. Rejected: "readable puppet IDs
removed": an artifact of diffing against a base that had moved on, not part
of this change.

Second pass, adopted: a `gh pr` body file must be an existing regular file
of the watched repository's own checkout, never git metadata (it is
published under the Agent's token, now even under Deny; this narrows the
Claude path too); a queued repo step never starts while a stop is pending
or after it was accepted. Rejected: re-validating the whole dispatch inside
the worker before a pinned command starts. The step starts right after its
check on the same turn; a push still rechecks the watch right before it
publishes, as for Claude. A policy saved in that window applies from the
next call (residual).
