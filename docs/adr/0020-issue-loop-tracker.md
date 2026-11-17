# ADR 0020: Issue loop tracker (follow each issue through to merge)

Status: accepted; implemented on `feat/issue-loop-tracker`.
Builds on ADR 0016 (work list, claims, review loop) and ADR 0017 (no roles).

## Context

The issue watch treated "seen" as "done": the poller posted the hand-off,
recorded `issueSeen`, and never looked at the issue again. On Nov 16 in the
demo sandbox, issue #31 got PR #32 four minutes later but nobody reviewed or
merged it for 18 hours, and issue #33 was handed off just before the servers
restarted; its run died and nothing ever retried it.

## Decision

Each handed-off issue gets one record in this device's ledger
(`issue_track`, owner API part `track`), with a small state machine
(`hagency_agent_local::issue_track`, pure `decide` and `apply_change`):

    handed_off(attempt, at) ──linked PR open──▶ pr_opened ──review asked──▶ reviewed
         │  no PR by deadline: re-hand-off (attempt+1)            │
         │  attempts exhausted ──▶ failed                         │
         │  Agent's "cannot fix" comment ──▶ needs_human          │
         └──────────── issue closed: completed ──▶ merged ◀───────┘
                       issue closed otherwise ──▶ closed

- **Hand-off** records `handed: 1` in the same transaction as `issueSeen`.
  Issues seen before this change and still open are adopted as attempt 1
  from the moment of adoption.
- **Retry.** Attempt *k* gets `HANDOFF_TIMEOUT_SECS · 2^(k-1)` (30 min for
  the demo) to produce a linked pull request; at most `MAX_ATTEMPTS` (3)
  hand-offs. A retry posts "🔁 Retrying #N (attempt k of 3)" and a new
  hand-off whose transaction id includes the attempt (the first keeps the
  old id). On the work list the GitHub claim is checked again first; a claim
  now held by another Agent stops the retry.
- **Linking** is read-only: an open pull request by the Agent's GitHub login
  whose description has a closing keyword for the issue, or whose head
  branch names it (`issue-33`, `issue33`, `33-…`). The hand-off now always
  asks for "Closes #N" and, if the Agent cannot fix it, an issue comment
  starting with `CANNOT_FIX_MARKER` and the reason.
- **Follow-through.** While the pull request is open, each check hands it to
  the existing review loop (`review_loop::advance`) with another of the
  owner's active Agents that has joined the room as reviewer. The loop posts
  once per head, then asks the merge gate (`merge_readiness`) and posts
  "Approved … merge with --match-head-commit", "needs a maintainer" or
  "needs the owner". With no other Agent, one "PR #M for #N needs your
  review and merge approval" line is posted, once. The tracker never merges,
  closes issues or comments on GitHub itself.
- **Done.** The issue closing as completed (from "Closes #N") posts
  "✅ #N fixed → PR #M merged" once and ends the record.
- **Once only.** Every tracker line is an `m.notice` (never dispatched as a
  request) posted before its state is recorded, with a transaction id fixed
  by Agent, repository, issue and step, so a check that dies in between
  repeats a duplicate the homeserver drops. The ledger never lowers an
  attempt count, counts a hand-off only as the next attempt, and never
  leaves a terminal state.
- **Restart safety** comes from the ledger: the poller reads the records on
  every check, so app or guardian restarts resume where they stopped.
- **Status.** The Agent's issue-watching line gains one part per tracked
  issue: "#33 retrying (2/3) · #31 PR #32 in review".

## Consequences

- A run that dies is retried at most twice more and then shown as failed;
  nothing retries forever.
- The tracker needs one extra GitHub read per active issue per check
  (comments, for the cannot-fix signal) plus one per repository (pulls).
- A pull request closed unmerged leaves the record waiting for a person;
  it is not retried, so a rejected approach is never re-attempted blindly.
- Reviewer choice is "another active Agent of this owner in the room";
  cross-owner reviewers keep coming from ADR 0016's review loop.
