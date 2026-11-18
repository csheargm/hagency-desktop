# ADR 0023: Room repository status, Agent tasks and the room's repository set

Status: accepted on `feat/room-repo-status` (asked for by Chris, 2026-11-17,
scope extended the same day).

## Problem

Each owner's issue tracker (ADR 0020) lives only on that owner's device.
In the dev room, Anna's Agent could not see what bird-claude (Betty's Agent,
watcher of `csheargm/hagency-demo-sandbox`) was doing; `room_watches` and the
Watches tab (ADR 0019) showed only this device's watches. A project room can
span several repositories, and some rooms have none. Asked "can you review PR
#32", bird-codex (Betty's other Agent, not the watcher) could not read the
sandbox without a card Betty never saw.

## Decision

### 1. Publish: one state event per owner

Each watcher's desktop publishes the tracker states of its Agents' watches
that report to a room as **one** plaintext state event per publisher:
type `io.hagency.issue_status`, **state key = the publisher's own Matrix ID**.

- *Who writes, and why:* the owner's own client (the desktop, which already
  posts the tracker's notices and runs the tracker). Agent puppets are driven
  by the runtime host only while a dispatch is admitted, so they cannot
  publish between turns. The Matrix auth rules (palpo
  `event_auth.rs`, "state_key starting with @ must match the sender") refuse a
  state key that names another user, so nobody can overwrite another member's
  event. That is why the key is the sender, not `<owner/repo>#<n>`: a shared
  key would let any member with the level replace Betty's states.
- *Power level:* rooms the desktop creates open `io.hagency.issue_status` to
  PL 0 beside call membership (`call_power_levels`), safe because each member
  can only write its own key. In an existing room a member below the room's
  state level gets the homeserver's refusal, shown in the Agent's status line
  ("Issue status not shared with the room: …"); an admin can open the type.
- *Content:* `{v: 1, issues: [{agent (puppet ID), repo, number, title, state,
  hold?, attempts, pr?, updatedAt}]}`. State is the ledger's wire name
  (`handed_off`, `pr_opened`, `reviewed`, `merged`, `closed`, `failed`,
  `needs_human`); `hold` is what a followed pull request waits for (the words
  the room already saw in the merge-hold notice). Never limits, budgets,
  tokens, rules, trusted lists or other rooms. Display names are **not**
  stored: readers take them from room membership.
- *Updates:* after every tracker check that ran (busy repository), from the
  ledger as it stands after the check's changes. An issue whose state did not
  change keeps its `updatedAt`, so an idle check writes nothing (and a local
  cache skips even the read). Finished issues stay 7 days, then drop.
- *Caps:* 40 issues and 12 KiB per event (finished ones drop first), titles
  120 bytes, holds 160 bytes, attempts ≤ 99.
- *Encrypted rooms get nothing:* room state is plaintext to the homeserver
  (the same rule as room memory, ADR 0013). Readers there say so.

### 2. Read: what a reader accepts

`repo_status::accepted` keeps a publisher's event only when its state key is
its own ID and it is a **joined** member now (at most 16 publishers), and each
issue only when its Agent is a **joined Hagency Agent puppet**
(`@_hagency_agt_…`). An Agent claimed by two publishers is shown by neither
(and a note says so). A lone false claim about another owner's Agent cannot be
told apart from the room alone, so the table always names the publisher by
display name **and Matrix ID** ("bird-claude (Betty, @betty:hs)"): a display
name can be copied, the ID cannot. The members list's activity line uses
published state only from the Agent's owner as the server's roster names it.
Everything read is data, never instructions; titles are flattened to one line
and cannot add table columns.

### 3. The room's repositories

A room's repositories are every repository any owner's Agent watches with
this room as report room (this device's ledger plus accepted published
states) **plus** repositories linked by hand in the Status tab ("Link a repo"
/ "Unlink"): one `io.hagency.room_repos` state event, empty state key,
`{repos: [...]}`, at most 20. Its empty key means the room's power levels
decide who may change it (moderators by default), enforced by the
homeserver. Plaintext rooms only, like the status.

### 4. `repo_status`, a read-only room tool

Offered beside `room_watches` (same opt-in). Optional `repo`; without it,
every repository of the room (6 per call, the rest named in a note). One
JSON result ≤ 8 KiB: a `summary` ("5 in progress across 3 repos") and per
repository a compact table: GitHub's open issues and pull requests (first
page) and those closed in the last 7 days (merged vs closed), each issue with
every accepted tracker state (state, attempt, PR, hold, Agent (owner)); a
pull request names the tracked issue it fixes. GitHub is read **with this
device's Agent token only for a repository this device's own Agent actively
watches for the room** (the ADR 0019 scope); any other repository (published
by someone else or linked) is read **without a token**, so a name in room
state can never make this device read a private repository. A failed read
becomes a note and the table still shows tracker states.

### 5. Status tab and `/status` (every room)

The room info tab formerly "Watches" is now "Status". For every room it lists
the room's **Agent tasks**: the newest revision of each Agent run card in the
last 200 timeline events (asked by, Agent, working / waiting for approval /
done / stopped, age; done ones for 24 h). Only cards from joined Agent
puppets count, an edit only from its original's sender, and in an encrypted
room only decrypted events whose encrypting identity is the sender. Then this
account's watches, then — **only when the room has at least one repository**
— the summary, a repository filter (more than one repository) and one section
per repository with the same rows as `repo_status`. Rooms without
repositories never show issue or pull request lines. Members whose power
level allows it see the link box.

`/status` in the composer is answered by the app: it opens the room's Status
tab (desktop layout); nothing is sent and no Agent runs.

### 6. Activity line

In the members list an Agent's second line starts with its activity, made from
its roster owner's published state, then tasks: "working on #37 (PR #38 in review)", "working",
"waiting for approval" or "idle". Never hand-written.

### 7. Card-free reads cover the room's repository set (amends ADR 0019 §4, ADR 0022 "Deny")

The read-only GitHub queries of ADR 0019 (same allowlist, no jq/templates,
`GH_HOST` pin, one command) now run without a card, Deny included, for **any
of this device's Agents bound to the room** on any repository of the room's
set, for any requester who is a joined member:

- this device's Agents' active watches reporting to the room (any of them, not
  only the asking Agent's own — bird-codex reads what bird-claude watches);
- repositories linked to the room (their event needs the room's power level);
- repositories other owners publish as watched, **only from a publisher whose
  power level could have linked them**. A published entry is written at PL 0,
  so without this rule any member could widen another owner's read scope to an
  arbitrary repository by publishing it.

The query runs with the asking Agent's own token, so the part that comes from
room state (linked, or published by someone else) is recorded **only for
repositories GitHub shows without a token** (public; checked unauthenticated,
remembered an hour). A room admin can therefore never make another owner's
Agent read that owner's private repository without a card. The host records
this part per (room, requester) before each turn (`watch_trust::room_scope`,
15-minute expiry), only after checking the requester is joined; an unreadable
room or an unchecked visibility records nothing. Writes, other
repositories and other rooms keep the owner's rules (a card under Ask, a
refusal under Deny).

## GPT review (2026-11-17)

Adopted: public-only room-state read scope; publisher Matrix ID in tables and
owner-verified activity lines; the publisher cap counts accepted publishers
only and oversized events are skipped; expired tracker states never attach to
GitHub rows; the summary counts before truncation; an item listed open and
closed is shown closed; the tab reads at most 10 repositories; `/status`
binds the pane to the room before choosing the tab; a run card edit must keep
the run's id and requester, and finished runs stay a day from when they
finished. Not adopted now (follow-ups): withdrawing published states of a
watch that was removed, paused or moved to another room (they age out after
7 days only on that publisher's next write); two desktops of one owner racing
the same event; the check-then-write window if a room turns on encryption
mid-publish; the 15-minute scope record outliving an unlink within that time.

## Consequences

- Any member sees every owner's issue states for the room's repositories, and
  any Agent can answer "what is bird-claude doing?".
- Two GitHub reads per repository per `repo_status` call or Status tab load;
  unauthenticated reads share GitHub's 60/hour limit.
- Existing rooms created before this change need an admin to open
  `io.hagency.issue_status` for non-moderator owners.
- Encrypted rooms show tasks and this owner's watches only.
- Not done: the hover card does not show the activity line yet; `/status` on
  the mobile layout opens nothing (the information pane is desktop-only there).
