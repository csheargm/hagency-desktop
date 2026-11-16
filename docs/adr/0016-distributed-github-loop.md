# ADR 0016: Distributed GitHub loop (work list, claims, maintainer merges)

Status: proposed; first milestone implemented on `feat/auto-loop-review`.
Design: `evaluation/design-distributed-github-loop.md` (Chris Xie, 2026-11-16).

## Context

On Nov 16 Betty's Agent (beta server, Betty's Mac) reviewed PR #30, but
Anna's merge still asked for a card: the merge gate only counted reviews
recorded in Anna's own ledger, and the review had been requested by hand. We
first sketched a shared room record (`org.hagency.review`) and an admin-set
room trust that switched on autonomy. The owner chose a simpler model,
closer to open source: **GitHub is the place where work is coordinated and
recorded, and it is the authority.** Every owner sets their own Agent's
watches. No room is required.

## Decision

### Work list (owner-set, per Agent, many repositories)

Each Agent has a list of `{repo, role, roomId, since, ...}` entries in this
device's ledger (`agent_work`, `merge_policy::work`). Only the owner API
writes it (`POST issue-watch {work: [...]}`, owner check plus execution-device
check), and no Agent tool can. The Agent panel's "Repositories and roles" block
edits it as one strict line per repository (`work_lines`). An unknown word
refuses the whole list. The roles are:

- **Developer.** Takes new issues (`trusted` authors by default, or `all`)
  opened since `since`. Before the hand-off the host posts a claim comment
  with the owner's own token (`CLAIM_MARKER`) and reads the comments again.
  The earliest unexpired claim (24 h) holds the issue, so two hosts on
  different servers end up agreeing. An issue someone else holds is marked
  seen and never handed off. Each Agent may claim at most
  `DEFAULT_ISSUES_PER_DAY` issues across all its repositories
  (`agent_work_claims`). The hand-off asks for "Closes #N" and for the pull
  request to be marked ready. `gh pr ready` and the Agent's own reviews skip
  the card for any non-protected path; merges never do.
- **Reviewer.** For every ready pull request opened by someone else (its
  author is not this owner's GitHub login), the host posts one "🔎 Review
  requested" notice per head commit, mentioning this owner's own Agent. The
  notice holds the exact head SHA and `REVIEW_STEPS`: plain reads that already
  run without a card (`gh pr view --json …`, `gh pr diff`), then
  `gh pr review --approve|--request-changes -b "Hagency verdict: …"`. The
  instructions live in one constant in `hagency-runtime::merge_note` (R3).
  The notice never copies the pull request's title, body or branch.
- **Maintainer.** Same as reviewer, limited to the owner's trusted authors.
  It then asks the gate for a dry-run decision (`merge_readiness`) on the
  exact head:
  - `merge`, and this owner's own GitHub permission (read with the owner's
    own token, never cached across owners) is admin, maintain or write: post
    "✅ Approved at <sha> — merging under the repo rules". This tells the
    Agent to run `gh pr merge N --squash --match-head-commit <sha> --repo R`,
    which the gate then answers by itself.
  - `merge`, but the owner cannot merge: post "needs a maintainer".
  - pending CI, no review yet, unreadable facts, or a moved head: wait for
    the next check.
  - anything else: post "needs the owner: <reason>", with the reason quoted
    as inert data.

  Each step happens once per (Agent, step, PR, head) (`review_loop_sent`). It
  is posted first with a transaction id fixed by that key, then recorded. A
  merge that GitHub refuses is reported by the Agent and is not retried. Stop
  All is checked before each step (`kill_switch::paused_here`), as the issue
  hand-off does.

The single-repository issue watch stays as it was, for backward
compatibility. One repository's failure (404, rate limit, no permission)
appears in the status line and does not stop the others.

### Maintainer merge rules (owner-side; the repository file can only tighten)

`OwnerRules::policy()`: docs-only (`**/*.md`, `docs/**`, ≤50 lines, or
`max=`) by default, or `any-path`. Every reported CI check must be green on
the exact head (pending: wait; red: a person decides; no checks at all:
allowed). These always need a person: `.hagency/**`, `.github/**`, any
`CODEOWNERS`, and the owner's `human=` globs. Renamed or opaque files also
need a person. If the base commit has a `.hagency/merge-policy.yml`, it is
judged as well, and *both* must pass. Without rules, the file alone decides,
as before. With neither, the card asks.

Separation and approvals:

- When the PR author's GitHub login is not this owner's login (Betty's PR
  reaching Anna), the maintainer Agent's own approval of the exact head
  counts. That Agent did not write the change.
- When this owner's login wrote the PR, no approval from this device's own
  Agents counts under the owner's rules, because they all share that login
  and any of them may be the author. Only a trusted reviewer's bound GitHub
  approval, or the owner, can carry it. The legacy file-only path still
  excludes only the merger (the existing `TODO(pr-author)` in
  `merge_policy.rs`).
- An optional `reviewers=` list counts a trusted login's GitHub review when
  that login's latest decisive review APPROVES exactly the merged head. It
  never counts the PR author's own review. An unreadable review list counts
  nothing. An APPROVED review whose body does not name the commit GitHub
  attached it to reads as `UNBOUND` and never counts: `gh pr review` cannot
  pin a commit, so a push between the reviewer's read and its review would
  otherwise attach an approval to an unread head.
- The author must be on `authors=` (`*` = anyone), or be this owner's own login.
- An approving review skips the card only if its body names the current head
  SHA (`review_names_head`). This is the commit the reviewer read; an
  approval of an older head asks the owner. For a maintainer, the author must
  also be trusted.
- `--request-changes` withdraws that Agent's settled and pending approvals of
  the pull request before anything answers the command (the gate or the
  owner's card).
- The repository file keeps its own approval rule: an Agent other than the
  merger, counted from local approvals only. The owner's relaxation and
  `reviewers=` apply only to the owner's policy. This device's own login in
  `reviewers=` is never counted a second time.
- Missing CI evidence (no `statusCheckRollup`) is treated as "still
  running", never as "no checks".
- A developer's claim is reserved in the ledger before anything is posted on
  GitHub. The reservation and the daily limit are one SQL statement. Asking
  for the same reservation again succeeds, so a failed hand-off is retried.
- Residual: `room_send::send_routed_with` awaits a room read after the last
  Stop All check, so a pause during that await can still let one notice go
  out. The merge itself is still judged by the gate.

The file format also gained `tier: trusted` (`paths:`, `max_lines:`,
`always_human:`), which always requires CI. Trusted-tier keys without
`tier: trusted` are refused.

## Threat model

- **GitHub is the authority.** Branch protection, required reviews and checks,
  and the owner's own permissions are enforced by GitHub. Hagency's gate is
  one more layer and never a weaker one. Each host uses only its own owner's
  token. No one's identity or permissions are emulated.
- **Injection through issue or PR text** can at most produce a PR. A merge
  still needs the maintainer's rules: trusted author, scope, protected paths,
  green CI on the exact head, and an approval. Notices carry only numbers, the
  repository, the SHA and quoted gate reasons.
- **A compromised Agent of a maintainer who chose `any-path`** can merge code
  that a trusted author wrote and it approved. The owner opted into this
  explicitly. Stop All, GitHub's branch rules and revert remain. Protected
  paths always need a person.
- **A moved head** voids approvals and readiness, because everything is keyed
  by the head SHA and the merge is pinned with `--match-head-commit`.
- **Claim races** are settled by GitHub's comment order. The worst case is two
  PRs for one issue, and that never leads to a merge without the rules.
- **Residual risks.** The review loop reads the newest 100 open pull requests,
  and claim comments up to the page cap (1,000). The `author` comes from `gh pr view`. A login rename
  between the read and the merge is not handled. An approval landing between
  readiness and merge is re-judged by the gate. The legacy per-agent issue
  watch keeps its single repository.

## Not yet done (follow-ups)

- Capability cards (`org.hagency.agent_capabilities` state in trusted rooms,
  the `agent_capabilities` tool) and the room-trust flag used for visibility
  or a "members of my trusted rooms" author list. These are shelved; the
  merge gate does not use them.
- The Create-agent dialog step. A per-Agent picker for issues per day and
  open PRs (only the daily claim cap exists today).
- Claim expiry clean-up (re-claiming once a PR exists). Fork PRs for owners
  who cannot push.
- The legacy issue-watch hand-off by itself still relies on host-side push
  (ADR 0015) for card-free pushes and PR creation.
