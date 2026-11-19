# 0030. One room work loop: autonomy level, Pause, room roles

Status: accepted (Chris, 2026-11-18: "make the settings easier, just one
click"). Supersedes in part ADR 0016 (watch modes, work list), 0022 (push
freely), 0025 ("fix issues without asking"), and ADR 0027 (trial mode).

## Context
"May agents do this on their own?" was answered in six places: the
issue-watch mode, "Fix, push and open PRs freely", the "Pull request merges"
switch, "Fix issues without asking" per group room, trial mode and the room's
merge rules (ADR 0029). Nobody could predict what would happen, and a room
had no single way to stop.

## Decision
A project room runs one work loop: **work source → claim → work → review →
deliver**. For GitHub: issues of the room's linked repositories → claim
comment (earliest unexpired wins) → branch, commit, push, pull request →
another Agent's review → merge.

### Roles (Matrix power levels, plain names)
- **Owner** (room creator or admin, power level 100): everything below, the
  merge rules (`io.hagency.merge_rules` stays at 100) and making or removing
  maintainers (Members → ⋯ → "Make maintainer" / "Remove maintainer", which
  set only that person's `users` entry to 50 or 0; never the owner, oneself,
  an Agent or a service account).
- **Maintainer** (power level 50): the autonomy level and Resume.
- **Member** (power level 0): Pause only.

### Autonomy level
Room state `io.hagency.room_autonomy`, state key `""`, content
`{"level": "off"|"assist"|"autonomous", "cleared": [<pause event IDs>]}`. It
counts only from a joined person who may send it now **and** has the
maintainer level (a permissive room's event level alone is not the role). Its
power-level entry is added at 50 (never below a stricter `state_default`,
an entry already there is kept) when a room is created, linked or set up.
- **Off** (no event): no loop; agents answer only when asked.
- **Assist**: agents claim, work and review; every merge waits for a person.
- **Autonomous**: merges happen on their own where the room rules allow.
New rooms are set to Assist by "Let agents work on a repo".

### Pause / Resume
A pause is room state `io.hagency.room_autonomy_pause` keyed by the pausing
member's own Matrix ID (power level 0; the homeserver lets nobody write
another member's key). A pause counts unless its event ID is in the level
event's `cleared`, whatever its content or timestamp: editing or redacting
one's own pause sends a new event, so a member can pause but never resume.
Resume is a maintainer's level event clearing exactly the pause events it
read, so a pause sent after the read still counts and no clock decides
(GPT review: an earlier timestamp design could hide an unseen pause). Pause and Resume
each post one room line: "⏸ Paused by Anna at 13:40" / "▶ Resumed by Ben at
14:02". Pause stops the loop's runs in progress in that room through the run
card's own Stop (only runs whose request is one of the loop's notices; plain
@mention answers keep running), nothing new starts, nothing is delivered.

### Reading and recording (fail closed)
The poller saves the derived work list before it records the rooms (so an
unlinked repository's push grant is gone first) and runs nothing in a check
whose recording failed. The owner's desktop reads each loop room's full state from the homeserver
every poll (never the sync store): the level event counts only from a joined
person (not an Agent or service account) the room's power levels let send it
now; linked repositories count likewise. It records per room through the
owner API (`roomAutonomy: [{roomId, level, paused, unread, ownerMaintainer}]`,
owner session, execution device; no Agent tool reaches it). The gate and push
trust judge by the snapshot only while it is younger than 300 s. Missing,
stale, unreadable or refused: the room counts as paused, and the reason is
said (status line, hold notice).

### Enforcement
- Desktop poller: only rooms that run (Assist/Autonomous, not paused) get
  claims, hand-offs, reviews and merge steps; each held room is named in the
  status line.
- Gate (agent-local): reviews and readying need the Agent to "may work" in
  the room and the room to run; a merge without a person needs Autonomous,
  and the owner of this device's Agents must be the room's owner or a
  maintainer (`ownerMaintainer`, from the server's power levels). A merge
  goes on (to the gate or the owner's card) only in a room with a fresh
  snapshot saying the owner is one, or in the Agent's own private chat with
  its owner (known from the server's binding, `scope_kind: owner_direct`,
  never from membership or the sync store). Anywhere else it is refused
  outright, card or not ("only agents of the room owner or maintainers merge
  here", or, with no fresh read, "merges happen only in a project room …");
  Assist holds it ("room is in Assist — a person
  delivers"). Merge commands the gate does not recognize keep the ordinary
  tool policy (a follow-up could enforce the role at the GitHub write itself).
- Push trust (fix steps, ADR 0022's pinned steps) runs for an Agent that may
  work in the room, for that room's linked repositories, while the room runs;
  for the owner and room members alike (replacing "fix issues without asking").
- Filing issues ("@agent go build this"): `gh issue create --repo <a repo the
  room links> --title … --body …` runs without a card when the request comes
  from the room's owner or a maintainer (power level at least 50, read from the
  server when the turn starts) in an unencrypted room (an encrypted room's
  discussion is private, so its requests keep the card) while the room runs (exactly those three
  options, each once, bounded lengths, rebuilt as single-quoted words on
  github.com). The asked Agent only files issues; other Agents pick them up
  through the normal claim, so no Agent assigns work to another. A plain
  member's request asks its owner through the usual card; posting the
  proposed issue list in the room for one approve click by the owner or a
  maintainer is a follow-up.

### Per Agent
Only "May work in <room>" (owner ledger, `mayWork`) and its budget. The
Agent's work list is derived from it: each may-work room's linked
repositories, push steps on, an existing entry's settings kept; the work
list always claims an issue before its hand-off, so two Agents never fix one
issue. The owner's "Stop all agents" stays.

### One-step setup
Room info → Status → "Let agents work on a repo": links the repository (and
adds the room's missing Hagency power-level entries this account may add),
turns "may work" on for this account's Agents in the room, sets Assist when
the room is Off (owner or maintainer), fills the suggested merge rules when
there are none (owner), and ends with one line saying what was done.

### Migration (one-time, idempotent)
An old single-repository watch or work list becomes: its repository linked
to its room, "may work" on, and (only where the room has no level and this
account may set it) the level Autonomous when the Agent had "push freely"
and "Pull request merges" on, else Assist. Trial rows are ignored. Two
phases: every room's repositories are linked first; only then is "may work"
turned on, the old watch switched off, the Agent recorded as moved on this
device (`room-work-moved.json`, so unticking every room later is never
undone) and each moved room told in one line. A room that refuses (no right
to link) is reported in the status line and the move retried, so nothing is
dropped silently. Old entries get push steps like every may-work entry.

Making or removing a maintainer rechecks, on the server's power levels read
just before the write, that this account is the room's owner and the target
is not; one change at a time from a desktop. Matrix has no conditional state
write, so a change from another client in between can still be lost.

## Removed
The Issue watching box (mode, repository, room), "Fix, push and open PRs
freely", the "Pull request merges" switch, "Fix issues without asking", the
"Repositories" work-list block, trial mode (code and API).

## Consequences
- One place answers "what will agents do here, and how do I stop it".
- Merges need the owner's desktop running (it refreshes the snapshot); with
  it closed, every step falls back to a person within 5 minutes.
- Agents of plain members build and review but never merge; handing their
  merge to a maintainer's Agent is a follow-up (today a person merges).
- The room's discussion (summaries, "go build this") is the next work source:
  it plugs into the same loop and the same level.
