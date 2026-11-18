# 0024. Shared-agent defaults in group rooms; review by a different model

Status: accepted (Chris, 2026-11-17)

## Context
An owner who adds their Agent to a project or group room (the "Allow in
group rooms" consent and bind path) wants it to act as a team member there:
answer the members' questions, review their pull requests, and fix things
only with the owner's OK. Before this, every room member's PR review needed
a card on the owner's screen, so a review asked in the dev room waited for
the owner. And the issue loop asked whichever of the owner's other Agents
was in the room to review a pull request, often an Agent on the same model
as its author, which tends to share the author's blind spots.

## Decision 1: shared-agent defaults, kept by the owner
Per Agent and per room, in the OWNER's device ledger (`room_share` table,
`hagency_agent_local::room_share`), never room state: only the owner API's
approval settings (`{roomShare: {room, answer, review, fix}}`, GET lists
`roomShares`) changes them. A room the owner saved nothing for has these
defaults:

| What | Default | Where it is enforced |
| --- | --- | --- |
| Who may ask | room members | the existing request policy |
| Questions, summaries, status (`answer`) | allowed | `Ledger::request_disposition`: off makes a member's request wait for the owner's OK (the existing request card); the owner's own requests never wait |
| Read the room's repositories | no card | ADR 0019 read-only queries (and feat/room-repo-status's room repository set once merged) |
| PR review (`review`) | allowed, no card, for room members | `watch_trust::review_step` (below) |
| Fixes: edit, commit, push, PR (`fix`) | follow the owner's rules (a card) | `watch_trust::pinned`: on, a member's fix steps get the owner's own push-freely trust (ADR 0022), and only on repositories where the owner turned push freely on |
| Merge | the repository's merge rules only | `merge_policy` (unchanged) |
| Anything outside the room's repositories | asks the owner | unchanged |

The kill switch (Stop All), the budget and the Requests policy still apply:
nothing here runs before a dispatch is admitted, and a Requests Deny for a
requester still refuses everything, these shortcuts included.

The agent panel's Settings tab has an "In group rooms" card (room choice,
"Answer questions", "Review PRs", "Fix issues without asking"), and the
About card plus the hover card of an own Agent in one of its group rooms say
it in one line: "Shared in dev room: answers questions, reviews PRs; fixes
need Betty's OK" (en, zh-CN).

### The review post allowlist
A card-free review is exactly one command: `gh pr review <N>` with exactly
one of `--comment|-c`, `--approve|-a`, `--request-changes|-r`, exactly one
`-b|--body <verdict>` whose text starts with `Hagency verdict:`, and exactly
one `-R|--repo <owner/name>` naming a room repository. No body file, editor,
URL, host, other option, chaining, pipe, redirect (`2>&1` included),
substitution, heredoc or env prefix. It is a foreground Claude `Bash` call or
a Codex `watched_repo_step`, like the read-only queries. What runs is
REBUILT from the parsed words, every value single-quoted, with
`GH_HOST=github.com` and git's helpers off, so the posted body is exactly
the verdict that was checked and nothing the model wrote is run as shell.
Like the read-only queries (ADR 0019 "Anyone in the report Room"), it
overrides the high-risk tool rule inside that exact scope, Deny included,
while the dispatch still checks out (binding, expiry, no policy saved since
admission, requests not refused).

The merge gate's review record is unchanged: an approval counts toward a
merge only when the gate itself answers the review (agent auto-merge on,
the change low-risk under the policy, CI passing, the body naming the head
commit), as before. A card-free review that the gate does not qualify is a
GitHub review only; GitHub's own record (`review_settled`) still lets the
tracker see that the reviewer answered.

### Threat model
- Prompt injection from a room member (or text the Agent reads) can make the
  Agent post a review verdict on a room repository under the owner's GitHub
  token: a comment, an approval or a change request. It cannot merge
  (merge rules unchanged), post anywhere else, comment on issues, or edit
  anything. An approval from this route does not count toward a Hagency
  merge unless the gate qualifies it as before. GitHub refuses
  `--approve` on a pull request from the same account.
- With "fix" on, every member of that room gets the owner's push-freely
  capabilities on the push-freely repositories (ADR 0022 threat model,
  widened from the owner to the room's members). It is off by default and
  per room.
- "Review PRs" defaults ON, including for rooms an owner added the Agent to
  before this change. Chris approved the default; the owner turns it off per
  room in Settings.

## Decision 2: review by a different model
A room setting, room state `org.hagency.room_review` (empty state key,
`{"differentModel": bool}`), that any member whose power level allows the
state event (an admin) changes from the room information pane's Watches tab.
Never set means ON, so every project room starts with it on (only the
issue tracker reads it, for a watch's report room).

When the issue-loop tracker (`tracker_live::pick_peer`) asks for a review of
a pull request its Agent opened, it gathers the candidates joined to the
room: this owner's other Agents (runtime from their saved model profile on
this device) and every other owner's Agent on the project's room roster (any
server; runtime from the roster row once the server publishes it, unknown
until then). With the setting on and the author's runtime known, it picks
the first candidate on a different runtime (a Claude author gets a Codex
reviewer and the reverse), this owner's Agents first. With none, it falls
back to the first candidate (own Agents first, since their approvals are in
this ledger) and posts once, when the review is first asked for: "No Codex
reviewer in the room; asked anna-claude". Off, or with the author's runtime
unknown, it asks the first candidate quietly; nobody at all still asks a
person.

## Open questions
- The server's room roster does not publish an Agent's runtime, so another
  owner's Agents are "unknown runtime" and serve only as fallbacks until it
  does (`RosterRow::runtime` reads `runtime` when present).
- Other members see the one-line sharing summary only once it is published
  (for example on the roster); today the hover card shows it for your own
  Agents only, since the choices live in your ledger.
- A cross-owner reviewer's approval counts toward a Hagency merge only when
  its GitHub login is a trusted reviewer in the owner's rules.
- Should "Review PRs" respect a saved high-risk Deny instead of overriding
  it like the read-only queries? Default now: it overrides (as ADR 0019).

## GPT review (2026-11-17)
Adopted: the host's last check right before a room member's push rereads the
owner's "fix" choice (turning it off stops a push already authorized); the
auto-merge gate also skips the card for a member's review only while
"review PRs" is on in that room (the owner's own review loop is unchanged);
a review's PR number is 1..=i32::MAX (gh reads a larger one as a branch
name); a saved room share names a Matrix room ID; roster fallbacks are
sorted so every check picks the same one; an unreadable room setting uses the
value last read for the room before the default; the Watches switch takes
this account's room handle before it reads or writes.
Rejected or left open:
- "answer off" is enforced where the runtime decides whether to call the
  model (`request_disposition` before the request card), not by a
  dispatch-scoped approval record at admission. That is the only path that
  starts a model call; a switch saved after that check applies from the next
  request (residual).
- A room share may be saved for a room the Agent has not joined yet: the
  owner's own choice through the owner API, kept across leave and rejoin.
- The fallback notice is said when the tracker first records the review as
  asked; a reviewer that changes between two checks before that, or a review
  answered before the first record (CI wait, hold), can leave the line
  missing or naming the new pick (residual; the review request itself names
  the reviewer).
- The pre-existing withdrawal of an Agent's approvals on a request-changes
  proposal (before it runs) is unchanged here.
- Cross-owner runtimes stay unknown until the roster publishes them (open).
