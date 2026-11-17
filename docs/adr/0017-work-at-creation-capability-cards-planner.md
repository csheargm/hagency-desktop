# ADR 0017: Work at creation, capability cards, and the planner

Status: accepted on `feat/auto-loop`.
Design: `evaluation/design-distributed-github-loop.md` ("From a description
to software", "Set at creation", "Capability discovery", "Two ways of working").
Builds on ADR 0016 (the owner's work list).

## 1. "What should it work on?" at creation

The Create agent dialog has an optional step: repositories (one per line,
`owner/name [developer|reviewer|maintainer]`, Developer by default), a GitHub
token (optional when `GH_TOKEN`/`GITHUB_TOKEN` is set), limits and an activity
room. Empty means a plain chat Agent, as before.

- **Checked before the Agent exists.** Create reads `GET /repos/{repo}` with
  the owner's own token for each repository. A 404 (missing, or invisible to
  this token) refuses the step; Maintainer needs `admin`, `maintain` or
  `write` (`CAN_MERGE`). Nothing is created until every line passes, so a typo
  never leaves a half-configured Agent. The permission is read from the
  repository's own `permissions` object, not the collaborator endpoint, which
  answers 403 to non-collaborators of public repositories.
- **One work list.** The result is saved through the same owner API call the
  Agent panel's "Repositories and roles" card makes (`{work}`), so the panel
  shows and edits it, and the Agent can never write it. The typed token is
  stored as the new Agent's GitHub access (Keychain, this device only).
- **Limits.** `issues=N` (issues per day; the smallest value on the list caps
  the Agent across all repositories, default 5) and `prs=N` (no new claim
  while the owner's login has N open pull requests in that repository, default
  3 when set from the dialog) are new work-list fields, 1–100, also editable
  as text in the panel. The token budget is the existing Policy daily budget.
- **Activity room.** Notices go to the chosen room (the Agent is added to it)
  or, by default, to the Agent's private chat with the owner.
