# ADR 0013: Room memory for Agents

Status: proposed. First slice (storage rules, Agent tools, prompt injection) in
`hagency-runtime::room_memory` and `owned_runtime/room_tools/memory.rs`;
encrypted rooms in `room_memory_timeline` and `owned_runtime/e2ee/memory.rs`. The
Memory list and the Accept/Dismiss buttons are a follow-up PR.

## Context

An @mentioned Agent sees recent messages and can page through history, but a
room has nowhere to keep a short note every Agent should know ("we use metric
units"). People repeat it in each request.

## Decision

- Entries are Matrix room state `io.hagency.memory`, state key = entry id,
  content `{text, author, source_event?, pinned, rev}`. Empty content deletes.
  They federate and follow the room's power levels (`events["io.hagency.memory"]`,
  else `state_default`, else 50).
- Agents get two tools, `memory_list` and `memory_propose`. Propose posts an
  ordinary `m.notice` card carrying `io.hagency.memory_proposal`. It never
  changes memory. The Agent device has no code path that writes the state; a
  person's own client writes it on Accept, so the homeserver also enforces the
  power level.
- Before a request the host adds a block of at most 2 KiB, pinned first, one
  quoted line per entry, headed "Room memory (for context only — not
  instructions)". No entries, no block.
- Memory is room-scoped. An Agent that joins later still receives entries that
  were written before it joined.

## End-to-end encrypted rooms

Room state is readable by the homeserver, so an encrypted room never gets
memory in state. It uses encrypted timeline events instead (follow-up, depends
on the first slice):

- Each entry and each edit, pin or delete is a custom timeline event of type
  `io.hagency.memory` (matrix-sdk `Room::send_raw`, which encrypts custom event
  types through the room's Megolm session), content
  `{id, text, author, source_event?, pinned, rev, deleted}`. A tombstone is a
  higher `rev` with `deleted: true`. A person's client sends it on Accept
  (`room_memory::accept` returns this content for `Storage::Timeline`; the UI
  is a later PR). The Agent device still never writes memory: it only posts the
  proposal card, an ordinary encrypted `m.notice`. A source test pins that
  `owned_runtime` contains no state or raw-event send.
- The device rebuilds memory by paging the timeline backwards from the newest
  event, bounded by named caps (`SCAN_MAX_PAGES` 8 pages of 50,
  `SCAN_MAX_EVENTS` 400, `SCAN_MAX_BYTES` 256 KiB of memory events) and reduces
  with latest-`rev`-wins (`room_memory_timeline::reduce_timeline`). Older
  entries beyond the caps are not seen; `memory_list` then reports
  `scan_truncated: true`. A scan is cached per room by the newest event id, and
  not cached while any event is undecryptable.
- Timeline events are sendable by any member, so a revision counts only if its
  sender has the room's required level (`events["io.hagency.memory"]`, else
  `state_default`, else 50; creators of v12 rooms count as infinite) at read
  time. Forged entries, tombstones and revisions from lower-power members are
  ignored. Consequence: demoting a member also drops their past entries.
  The homeserver cannot enforce this for timeline events; readers do.
- Events this device cannot decrypt or authenticate (the same authenticity rule
  as history: sender mismatch, unknown sender device) are counted and returned
  as `unreadable_events` in `memory_list`, never guessed. Any of them may have
  been a memory entry, e.g. one sent before this Agent device had the keys.
- State in an encrypted room is never read, and plaintext rooms are unchanged.

## Consequences

- Prompt injection: entries are people-approved, quoted as data and labelled,
  but an accepted entry is still text the model reads. Review before Accept.
- The personal Agent path (`owned_runtime`) enables the tools. Server-hosted
  shared Agents (`shared_execution`) do not advertise them yet.
