# 0021. "Push freely" covers the fix steps of an issue

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
   owner (the issue-watch hand-off is the owner's notice), foreground Claude
   only (Codex cannot have its command rewritten, so it keeps asking).
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
- An explicit high-risk policy of Deny is still a Deny: push freely narrows
  what Ask-owner asks, it does not override a refusal.

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

## Open questions
- Should build/test scripts (`cargo test`, `npm test`) be card-free? Default
  NO: they run repository code (build scripts, test fixtures, npm lifecycle
  scripts) with the Agent's rights and token in reach; prompt injection
  plus a malicious PR branch would be code execution on the owner's
  machine. A sandboxed test runner would be the way to allow them.
- Hosted Agents default to a Deny high-risk policy (nobody sees cards). If
  the autonomous loop must run there, the owner needs a policy that asks or
  allows for that room; push freely does not lift a Deny.
