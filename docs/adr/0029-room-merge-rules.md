# 0029. Agent merge rules are room policy

Status: accepted (Chris, 2026-11-18). Supersedes the repository file
`.hagency/merge-policy.yml` of ADR 0016 and the policy-file line of ADR 0026.

## Context
The merge gate judged agent merges by `.hagency/merge-policy.yml` on the pull
request's base commit. That file is public (anyone can read a public
repository's rules) and means nothing to contributors who do not use
Hagency. The rules are Hagency policy of the team that works in the project
room, so they belong to that room: visible to its members, changeable only by
its admins.

## Decision
- **Where the rules live.** Room state `io.hagency.merge_rules`, state key
  `""`, content `{"policy": "<rules text>"}`. The text is the same small YAML
  subset the gate always parsed (`low_risk_paths`, `max_changed_lines`,
  `required_agent_approvals`, `require_ci`, `always_human`, `tier: trusted`,
  `paths`, `max_lines`), read by the same `parse_policy`; `render_policy`
  writes it and must parse back to the same rules (globs with quotes,
  commas, `#`, brackets or control characters are refused, never escaped).
  The type is not on the member-level (PL 0) list, so the room's
  `state_default` applies (50 under the Matrix presets: admins and
  moderators); the homeserver refuses anyone below it. A room that wants
  admins only sets `events["io.hagency.merge_rules"]` to 100.
- **Which room.** The room the watch of the repository reports to
  (`issueWatch.roomId` or the work list's `roomId`; paused watches never
  count): the merging Agent's own active watch, else the single room every watch of that repository on this device
  reports to; none or several: no room rules. The binding repository → room
  is the owner's own watch setting, never room content, so a room admin of
  some other room cannot claim a repository.
- **How the gate gets them.** The gate never reads Matrix. Every poll (60 s)
  the owner's desktop reads each watch room's full state from the homeserver
  (`GET /rooms/{id}/state`, never the sync store or a local echo), accepts the
  event only when its `type`, `state_key ""`, `room_id` and `$` event id
  match, its sender is not an Agent puppet or Hagency service account, is
  still joined (by the `m.room.member` state of the same server response; a
  kick or ban can leave a power level behind) and the room's power levels let
  that sender send the type now, and only while this account is joined; then it POSTs
  `roomMergeRules: [{repo, roomId, eventId, sender, policy}]` to the owner
  API (owner session, execution device). The ledger stores one row per
  repository and room (`room_merge_rules`: event id, sender, text, read at).
  No Agent tool reaches this path. A merge is judged by that row only while
  it is younger than `ROOM_RULES_TTL_SECS` (300 s); older, missing, invalid,
  oversized (over 8 KiB, stored as invalid so they replace older rules) or
  refused rules need a person, with the reason
  "no merge rules in this room — a room admin can set them in Room info →
  Status" (or the stale/invalid variant) in the room's hold notice.
- **Judging.** The room's rules are required for every merge and judged
  together with the owner's own rules (ADR 0016); each can only tighten.
  Without owner rules, approvals count as the file's did: another of this
  device's Agents than the merger, in this room. With owner rules, the room's
  rules count the same approvals the owner's rules do (an outside author's
  pull request may be approved by the maintainer that merges it; trusted
  GitHub reviewers count), so the distributed maintainer flow keeps working.
  Readying and approving reviews skip the card only when the room's or the
  owner's rules exist and allow the change.
- **Protected paths.** `.hagency/`, `.github/` and any `CODEOWNERS` always
  need a person, as before. A `.hagency/merge-policy.yml` in a repository is
  ignored; the gate no longer reads any repository file.
- **UI.** Room info → Status → "Merge rules": the rules in plain lines for
  every member, "Set by <admin>"; an account the power levels let send the
  type gets an editor (low-risk paths, max changed lines, approvals, require
  CI, Save). The editor starts from `SUGGESTED_LOW_RISK_PATHS` (one constant):
  `**/*.md`, `docs/**`, `*.txt`, `AUTHORS`, `CONTRIBUTORS`, `MAINTAINERS`,
  `CHANGELOG`, `NOTICE` (owner, Nov 18: root doc files are trivially low
  risk), with `SUGGESTED_ALWAYS_HUMAN` `**/LICENSE*`, `**/COPYING*`,
  `**/requirements*.txt` (legal files and dependency lists stay with a
  person). The Safeguards card's line reads "Merge rule (room policy): …".

## Consequences
- A pull request cannot loosen its own rules any more than before: the rules
  are not in the repository at all.
- Merges need the owner's desktop running (it refreshes the snapshot); with
  it closed, merges fall back to the owner's card within 5 minutes.
- A room admin of the watch room decides what Agents may merge in that
  repository on this owner's device; the owner still controls the
  per-Agent "Pull request merges" switch, the trial (ADR 0027) and their own
  rules.
- Behaviour change: an owner-rules maintainer without room rules no longer
  merges by itself; the room needs rules.
- Migration: set the room's rules (Room info → Status → Merge rules) before
  removing a repository's old file; the file has no effect either way.
