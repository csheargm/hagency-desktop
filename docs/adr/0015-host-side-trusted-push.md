# 0015. Host-side trusted push and fix steps

Status: accepted (demo), 2026-11-16

## Context
With issue-watch push trust on, Hagency answered `git -C <checkout> push -u
origin <branch>` without a card by rewriting it to a pinned command (explicit
https URL, `-c` pins). GPT review (P1): the rewritten command is still run by
the Agent's shell and git, and git always reads the checkout's `.git/config`.
The Agent can edit that file at any time (a background job, a later step), so
`url.*.insteadOf`, `credential.helper`, `core.sshCommand` or an `include.path`
could redirect the push or run a program while the owner's GitHub credential
is in reach. No `-c` set can neutralise every key git might honour.

## Decision
The host runs every network or branch step push trust answers; the Agent's
tool call is then rewritten to `printf` of the outcome (or a failing exit).
- Push: the host reads the branch's commit from the checkout (a local
  read-only query, helpers off), creates a fresh bare repository in its temp
  directory (`create_dir`, so nothing planted is reused; `--template=` empty),
  links the checkout's objects with `objects/info/alternates` (object files
  only, never config), sets the ref, and pushes
  `refs/heads/B:refs/heads/B` to `https://github.com/<watched>.git`.
- Fetch: the same scratch repository, seeded with the checkout's
  `refs/remotes/<remote>/*` (haves), fetches `+refs/heads/*` over https; the
  resulting pack is copied into the checkout and the tracking refs are
  updated with a local `update-ref --stdin` (hooks off).
- New branch (`checkout -b <new> <remote>/<default>`): no network, so it
  runs as the Agent (never in the host process), pinned: attributes from the
  empty tree (`GIT_ATTR_SOURCE`), `core.attributesFile=/dev/null`, hooks,
  fsmonitor and lazy fetch off, and only when the config holds data keys and
  there is no `info/attributes`, so no filter driver is selected.
- The push refuses the remote's own default branch (from `ls-remote
  --symref`, not the checkout's forgeable `origin/HEAD`). Host writes into
  the checkout use `update-ref --no-deref` and `create_new` files, and only a
  plain clone (real `.git`, `objects`, `objects/pack`, no `commondir`) is
  served. Trusted edits resolve every existing link and refuse dangling ones.
- Every network git runs with `env_clear` (PATH, HOME, user, TMPDIR, LANG and
  gh's token/config variables only), `GIT_CONFIG_NOSYSTEM=1`,
  `GIT_CONFIG_GLOBAL=/dev/null`, `protocol.allow=never` plus
  `protocol.https.allow=always`, `credential.helper=` then
  `!gh auth git-credential` (the owner's gh), hooks/fsmonitor/ssh off, and
  `core.alternateRefsCommand=true` (by default git would run `for-each-ref`
  inside the alternate, i.e. the checkout).
- Workspace file edits (Write/Edit/MultiEdit inside the workspace, never git
  metadata) are allowed in the watched Room, so the fix needs no card; the
  Agent note tells it to use those tools, not python/sed.

## Consequences
- Tests poison the checkout with insteadOf, pushInsteadOf, credential.helper,
  core.sshCommand, uploadpack.packObjectsHook, include.path and hooks, and
  show the push and fetch reach only the checked destination and run nothing.
- `-u` is recorded by the host (tracking ref and `branch.*` data keys).
- Fetches without haves for unknown history download more; fine for issue
  fixes. Stale remote-tracking refs are not pruned in the checkout.
- Residual: the checks before a pinned branch step and the edit target checks
  are not atomic against a concurrent writer; that writer already needs an
  owner-approved command, and runs only as the Agent.
- Codex stays untrusted: its approvals cannot replace the command with the
  host's report, so the Agent's own push would still run.
