# ADR 0017: Repositories at creation, no roles, and the planner

Status: accepted on `feat/auto-loop`.
Design: `evaluation/design-distributed-github-loop.md`. Amends ADR 0016.

## Decision: no roles

A general Claude or Codex Agent can do almost anything, so the owner chose
(2026-11-16) to drop Developer/Reviewer/Maintainer. Every Agent may fix
issues, review pull requests, file issues and plan, in every repository on its
list. **Merging is derived from the owner's own GitHub permission** on that
repository (`admin`, `maintain` or `write`, read each pass with the owner's
token), plus the owner's merge rules; GitHub's branch protection still
decides. Specialised Agents (video, graphics, voice) may later get explicit
specialities; that is not a role.

- Stored data stays compatible: `OwnerRules.role` is still read and written,
  but ignored. New entries are stored as `maintainer`; a role word in the
  panel text is accepted and dropped.
- Poller: each repository runs the developer step (claim, hand off) and then
  the review loop. Anyone's ready pull request is reviewed; only a trusted
  author's goes on to the merge step, and only when the owner's login can merge.
- Gate: readying and reviewing use the owner's rules with any path
  (protected paths still ask); merges use the full rules. An untrusted
  author's pull request may be reviewed card-free but never merged by it.
- Capability cards (shelved in ADR 0016) are dropped, not built.

## 1. "Repositories to watch" at creation

The Create agent dialog has an optional step: repositories (`owner/name`, one
per line), a GitHub token (optional when `GH_TOKEN`/`GITHUB_TOKEN` is set),
limits and an activity room. Empty means a plain chat Agent.

- **Checked before the Agent exists**: `GET /repos/{repo}` with the owner's
  token (`github_access::check::permission`). A 404 refuses the step; the
  result line says whether the owner can merge there (informational).
- **One work list**: saved through the same owner API call as the Agent
  panel's "Repositories" card (`{work}`); the Agent can never write it. The
  typed token becomes the Agent's GitHub access (Keychain, this device only).
- **Limits**: `issues=N` (smallest on the list caps the Agent per day,
  default 5) and `prs=N` (no new claim while the owner's login has N open pull
  requests there, counted with GitHub search so it is not limited to one page;
  default 3 from the dialog), 1–100. The token budget is the Policy daily budget.
- **Activity room**: the chosen room (the Agent is added) or the Agent's
  private chat with the owner.

## 2. The planner (description → issues → "Done")

- **Agent side.** A room tool `plan_propose {repo, title, issues:[{title,
  body, acceptance}]}` (both runtimes, personal Agents) posts a card: an
  ordinary `m.notice` carrying `io.hagency.plan` with a host-made id
  (`plan_` + digest of dispatch and plan, so a replay is the same card). The
  Agent files nothing.
- **Card.** The desktop draws the card only for an authentic Agent of the
  room's homeserver. Titles are editable (one per line; a kept line keeps its
  body and criteria by position). "File these issues" works only where the
  card's real sender is one of this account's own active Agents (matched by
  Matrix ID, never by anything in the card): the owner's approval is the
  click, and the issues are filed with that Agent's GitHub token. Each issue
  body ends with `Part of Hagency plan <id>, step k of n`. The room is told
  "Filed on repo: #12 …" (fixed transaction id).
- **State.** The filed plan `{id, repo, roomId, issues}` is kept in the
  owner's ledger (`agent_plans`) through the owner API (`{plan:{filed}}`); a
  second filing of the same id is refused. The poller checks each open plan;
  once every issue is closed as completed (a merged pull request with
  "Closes #N", or closed as done), it posts "Done: #A, #B merged (repo)" with a
  fixed transaction id, then marks the plan done (`{plan:{done}}`). Stop All
  holds both posts.
- **Follow-ups.** A requester who is not the Agent's owner can edit the card
  locally but only the owner files; "merged" is read as "closed as
  completed", not traced to the exact pull request. The timeline wiring adds
  three lines to the over-budget `room_screen.rs` beside the skill card,
  under its existing TODO(R4) to split message-card population.
