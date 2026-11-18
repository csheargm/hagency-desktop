# ADR 0019: Agents know and share their issue watches

Status: accepted on `feat/agent-watch-info` (approved by Chris, 2026-11-17).

## Problem

Asked "who is monitoring what repo?" in the dev room, an Agent said it could
not see whether issue watching was on: nothing told it its own watch. Asked
"any issues pending?", a watching Agent's `gh issue list` on its own watched
repository needed an approval card, and with the owner away it reported "the
owner's rules blocked" the command.

## Decision

1. **Own watches in the run context.** Each personal-Agent turn's prompt
   carries a host-data block (between `OWN_WATCH_OPEN` and `OWN_WATCH_CLOSE`,
   `hagency_agent_local::watch_info`) after room memory and skills: every
   watched repository, issues taken (trusted/all/labelled), the report room
   (marked "this room" when it is), active or paused, "push and open pull
   requests freely" on or off, and the merge rule in words. It comes from
   this device's ledger for the dispatch's own Agent only. A ledger that
   cannot be read gives no block (the turn still runs). Only watches that
   report to the turn's room are shown, whoever asks: the answer is posted to
   the room (GPT review).
2. **`room_watches`, a read-only room tool** (`hagency_runtime::room_watches`,
   opted in with `RoomTools::with_watches`). No arguments: the host uses the
   admitted dispatch's room, re-authorized like every room tool. It lists only
   watches whose report room is this room: Agent display name, owner, repo,
   active or paused.
3. **Room info "Watches" tab**, next to Memory and Skills, read-only, the same
   fields, from the owner API's Agents list and each Agent's saved watch
   (a watch of an Agent that is off reads "Paused").
4. **Read-only GitHub queries on a watched repository need no card**
   (`watch_trust::read_only`): `gh issue list|view`, `gh pr
   list|view|diff|checks`, `gh api` GET on the issue and pull request routes
   only, run with `GH_HOST=github.com`; no jq, templates or free-form search
   (gh's jq can read the token from its environment), foreground Claude
   `Bash` only, one command (no chaining, pipes, substitution, redirects or env
   prefix), options from a closed list, the watched repository named exactly
   (`-R/--repo`, or a `repos/<owner>/<name>/` api path). Authorization uses
   the same effective watches as the room view.
   The repository must be watched with reports to the turn's Room (an active
   legacy watch or a work-list entry). Any requester in that Room: the watch
   already reports that repository's activity there.
   **Anyone in the report Room, Deny included** (Chris, 2026-11-17 amendment).
   Such a query overrides the high-risk tool rule for every requester in the
   report Room, push freely on or off, Deny included, the same way push
   freely overrides it for the owner's fix steps (ADR 0022 "Deny"). The case
   that prompted this: Betty saved only her Agent's rule, so Anna's own
   requester and Room layers kept the default Deny. Anna's `gh issue list` on
   the watched repository was refused with no card. What stays: the dispatch
   must still check out (a Requests Deny for that requester, a stale
   proposal, or any rule saved after the run was admitted ends the shortcut),
   and edits, pushes, other repositories and other Rooms keep the owner's
   rules (a card under Ask, a refusal under Deny).
5. **An unanswered card is not a refusal.** When the card could not be shown,
   expired or was withdrawn, the Agent is told it is waiting for its owner's
   approval in their window and never that rules blocked it.

## Sharing scope (what is never shared)

The room view and tab carry no limits (issues per day, open pull requests),
budgets, tokens, trusted author or reviewer lists, always-human paths, label
names, or watches reporting to other rooms. Only the Agent's own block has the
merge rule and push setting, and only for that Agent.

## Limits

Amended by ADR 0023: the read-only queries cover the room's whole repository set (this device's Agents' watches for the room, linked repositories, and published watches from publishers who could have linked them), for any of this device's Agents in the room. Publishing watch states as room state is done there (`io.hagency.issue_status`).

Watches live in each owner's device ledger. `room_watches` and the tab show
the watches of this device's owner's Agents; another owner's Agents in the
same room are listed by their own owner's device. Publishing watches as room
state for every member is a later step. In the runtime, a watch's
active/paused reflects the watch setting; the tab also reflects whether the
Agent is on.

Open (GPT review, not adopted now): a host-side GitHub read returning only
room-shareable fields would bind the API origin and returned data more
tightly than an allowlisted `gh` command; PR patches and review bodies of the
watched repository are readable card-free in its report room.
