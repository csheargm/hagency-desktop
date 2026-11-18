# ADR 0028: Daily digest for issue watches

Status: accepted on `feat/daily-digest` (approved by the owner, 2026-11-17,
as part of the loop safeguards queue). Numbered 0028 because the Safeguards
card and trial mode branches each drafted a 0026 in parallel.

## Problem

The issue loop (ADR 0020) posts one room line per step. Over a day that is
many lines, and the ones that need the owner (a held pull request, an issue
the loop gave up on, an approval card nobody answered) scroll away among the
"in review" and "fixed" lines. The owner wants one message a day that says
what the loop did and what is waiting on them.

## Decision

Once a local day, each watched repository's report room gets one `m.notice`:

```
📋 acme/app daily digest — Today: 3 fixed, 2 merged, 3 need you
• #33 → PR #41 merged
• #34 → PR #41 merged
• #35 → PR #42 merged
• #36 → PR #43 needs you: https://github.com/acme/app/pull/43
• #37 gave up: https://github.com/acme/app/issues/37
• #38 approval card unanswered: https://github.com/acme/app/issues/38
```

### Source of truth: the tracker ledger

- **fixed**: tracked issues that moved to `merged` in the window (the tracker
  records `merged` only when GitHub reports the linked pull request merged
  and the issue closed as completed).
- **merged**: the distinct pull requests of those (one PR may close several
  issues, so merged ≤ fixed).
- **need you** = held + failed + unanswered:
  - *held*: records in `reviewed` with the notice flag set and a pull request
    — the tracker sets exactly that when the review loop posted its hold
    notice (the `review_loop_sent` hold step) or the "needs your review and
    merge approval" line. Held counts the current state, whatever day it
    started, because it still waits on the owner.
  - *failed*: records that moved to `failed` or `needs_human` in the window.
  - *unanswered*: issues whose hand-off run stopped at an approval card
    nobody answered in the window (the Activity view, this account's own
    hand-off requests only, as the tracker reads them), not already counted
    as held or failed.

The ledger had no time for a record's moves, so `issue_track` gains
`changed_at` (Unix seconds), stamped by `apply_change` whenever state,
attempt, pull request, notice flag or free-retry count changes. Older rows
read 0 and never count. `review_loop_sent` is not changed (no timestamps
there; the trial-mode branch works on the merge gate in parallel).

### The window

Each day's window is `[yesterday SEND_HOUR, today SEND_HOUR)` in local time
(`SEND_HOUR` = 18). Every moment belongs to exactly one day's digest, so a
digest sent late (the app started at 22:00) neither repeats nor drops moves,
and a clock change takes the earlier instant. A day the app does not run at
all after the send hour gets no digest; its moves are not carried over.

### Once a day, across restarts

The same post-then-claim rule as the tracker: the notice goes through the
tracker's room-notice path (`room_send::send_routed_with`, the tracker's
`io.hagency.issue_track` marker with `step: digest`, a transaction id fixed
by Agent, repository and day), then the owner API records the day
(`{"digestSent": {repo, day}}` → table `issue_digest_sent`, one row per
Agent and repository, never moved back to an older day). A check that dies
between the two repeats a duplicate the homeserver drops; the recorded day
stops every later check and restart. A failed post is not recorded and is
tried again next check.

### Quiet days and Stop All

A day without a move (and no unanswered card) posts nothing; the day is
still recorded, so later checks do not read the Activity view again. A held
pull request alone is not activity. While the Agent is paused (Stop All)
nothing is posted or recorded; the digest goes out after resuming that day.

### Who sends

Only the owner's own window: a signed-in account whose own Agent row it is
(no `ownerMxid` naming someone else), active and assigned to this live,
unrevoked device (`kill_switch::here`). A second window or device of the
same owner never posts a copy, and the owner API refuses the day record from
any device but the execution device anyway.

## Not done

- `/status` (ADR 0023) does not show the digest yet: it renders the room's
  published tracker states, and adding a summary there is a Status tab UI
  change that would collide with the Safeguards card work. TODO in
  `digest.rs` (`summarize` is pure and ready to reuse).
- Merges the review loop carried for pull requests that close no tracked
  issue are not counted: `review_loop_sent` has no time.
- The send hour is a constant, not a setting.
