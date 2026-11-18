# 0021 Owner approvals inbox

Status: accepted (2026-11-17)

## Context

Betty's bird-claude picked up an issue-watch hand-off in "dev room" (a room on
server alpha; Betty's account and the Agent's runtime are on beta / Betty's
own window). Its first shell step (`git fetch` plus a read of README) asked for
the owner's approval. The Claude transcript shows the call waited the full
280 s and then received the "approval card was not answered" answer, twice
(18:55 and 19:36 UTC). So the request did sit in the window's own runtime
queue for the whole time. Betty saw nothing.

Where a waiting request could be seen before this change:

- a card on the message that raised it. This app's own Agent names only its
  thread, never a message (`eventId` is empty), so the card appeared only on
  the thread root in the room timeline, and never in the thread's own
  timeline, which is where the owner was reading;
- the notice at the end of the open room;
- a popup that lasts 20 s, and a "●" after "Agents" on the rail;
- a "desktop notification" that was a no-op on macOS (`jit_compat`).

Every failure on the delivery path was silent: an Agent queue that could not be
read was skipped with `continue`, a failed app-wide reading was dropped by
`apply`, and a reading that hung blocked nothing and said nothing. The window
log therefore had no line about approvals at all. An expired request simply
vanished.

## Decision

1. An owner approvals inbox (`src/hagency/owner_inbox.rs`) floats over every
   page while anything waits. It lists each request with its room, Agent and
   exact command, and offers Approve once / Always allow this / Deny / Open
   room. It reads only the existing `pending_approvals` store and decides
   through `pending_approvals::decide`, so it grants nothing new. A new request
   reopens it if the owner hid it.
2. Counts: the Agents rail says "Agents N ●"; the Agent's card on the Agents
   page says "N waiting for approval".
3. A real macOS notification (`shared::desktop_notification`, `osascript` with
   the text passed as arguments, never as script) for every new request and
   every expiry.
4. A request that leaves the list unanswered within 15 s of its expiry is kept
   for an hour in `expired_approvals`: listed in the inbox as "Expired, not
   answered" until dismissed, shown on its message card in the Expired state
   and in the room notice. It is stored as already ended, so it can never be
   approved.
5. The card on a thread root also shows in the thread's own timeline when the
   request names no message of its own.
6. Nothing on the path is silent any more: a failed or timed-out reading
   (20 s bound, one reading at a time) and each Agent whose queue could not be
   read are logged and named in the inbox header.

The approval policy (what asks, what is trusted) is unchanged; that is owned by
the push-freely work.

## Consequences

- If the root failure recurs, the window log now says which step failed
  ("Pending approvals unreadable: …", "Approvals of Agent … unreadable: …"), and
  the owner sees the same in the inbox.
- `jit_compat::show_notification` now shows on macOS for its other callers too
  (Agent questions, admission requests), which was always its stated intent.

## Amendment (Chris's test, 2026-11-17): bounds per half, and the requester's view

Observed: Betty's window logged "Pending approvals unreadable:
approvals_read_timeout" while both servers' agent services stalled for about
30 s (lease renewals answered 502/503; Postgres logged a client connection
reset with an open transaction; the server logged
`database_connection_discarded`). The single 20 s bound covered the whole
reading, and both halves waited on the server: this device's own queue began
with the owner API's Agent list (`GET owned-agents`). Nothing was hidden that
time (the request was refused by policy, ADR 0019), but a waiting card could
have been.

Decision (`approvals_read`): the management list (8 s) and this app's own
queues (10 s in all, 2 s per Agent) are read at once, each with its own bound,
for the app-wide poll and the open room's notice alike. The own queue uses the
Agent list when it answers within 5 s, else the last list that answered for
this identity, and then names `agent_list_unavailable` (an Agent started
since may be missing). A slow server now costs only the management rows, and
the inbox names it (`managementError`).

The requester's view: a run that waits for its owner's approval shows the
step `APPROVAL_WAIT_STEP` on its run card (a room message every member reads;
it names no tool or argument) until the owner answers, the request expires or
is withdrawn. Any viewer reads "Waiting for <owner>'s approval (N s)"; only a
viewer whose app holds the actual request reads "Waiting for your approval"
with "Open approval" (an Agent writes its own card, so the step alone never
asks anyone to act, GPT review). Before a card exists, the requester's own message naming an
Agent shows "Waiting for <Agent> to start… (N s)" for up to 10 minutes or until
the thread has a reply. That line comes only from the requester's own message.
