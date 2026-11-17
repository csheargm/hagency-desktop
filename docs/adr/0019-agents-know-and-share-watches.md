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
   cannot be read gives no block (the turn still runs).
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
   list|view|diff|checks`, `gh repo view`, `gh api` GET, foreground Claude
   `Bash` only, one command (no chaining, pipes, substitution, redirects or env
   prefix), options from a closed list, the watched repository named exactly
   (`-R/--repo`, `gh repo view <repo>`, or a `repos/<owner>/<name>/` api path).
   The repository must be watched with reports to the turn's Room (an active
   legacy watch or a work-list entry). Any requester in that Room: the watch
   already reports that repository's activity there.
5. **An unanswered card is not a refusal.** When the card could not be shown,
   expired or was withdrawn, the Agent is told it is waiting for its owner's
   approval in their window and never that rules blocked it.

## Sharing scope (what is never shared)

The room view and tab carry no limits (issues per day, open pull requests),
budgets, tokens, trusted author or reviewer lists, always-human paths, label
names, or watches reporting to other rooms. Only the Agent's own block has the
merge rule and push setting, and only for that Agent.

## Limits

Watches live in each owner's device ledger. `room_watches` and the tab show
the watches of this device's owner's Agents; another owner's Agents in the
same room are listed by their own owner's device. Publishing watches as room
state for every member is a later step. In the runtime, a watch's
active/paused reflects the watch setting; the tab also reflects whether the
Agent is on.
