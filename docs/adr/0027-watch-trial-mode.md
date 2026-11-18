# 0028. Trial mode for a new watch

Status: accepted (Chris, 2026-11-17, "Loop safeguards queue")

Superseded by ADR 0030: trial mode is removed (its rows are ignored); a room
starts at Assist, where a person delivers every merge, until its owner or a
maintainer chooses Autonomous.

## Context
A new issue watch can merge low-risk pull requests by itself as soon as the
owner turns on "Pull request merges" and the merge rules pass. An owner who
just set up a watch has not yet seen how this Agent works on this
repository, so the first merges should be theirs.

## Decision
- A watch created from now on starts a trial: its first `TRIAL_PRS` (3) pull
  requests need the owner to merge, even where the merge policy would let an
  Agent merge them (`hagency_agent_local::merge_policy::trial`). The Agent
  still opens, readies and reviews them; only the merge is held.
- Where: the merge gate's `review_loop::judge`, after every other rule
  passed, so both the gate's own answer (`qualifies`, the card stays the
  owner's) and the loop's dry run (`merge_readiness`) say the same. The
  hold applies to every Agent's merge in the repository while any watch of
  it on this device is in its trial: every Agent shares one GitHub login, so
  who wrote a pull request is not reliably known before the tracker links
  it, and another Agent past its trial must not merge a trial watch's work
  (GPT reviews). The
  reason is `trial: pull request k of N needs the owner to merge`
  (`reason_kind` "trial"); the desktop's merge step posts it once per head
  through the existing hold notice: "Trial: PR k of N needs you to merge —
  after N owner merges this watch merges low-risk PRs by itself." (en, zh-CN).
- The count is the owner's, in this device's ledger (`watch_trial`,
  `watch_trial_merges`). A pull request counts when the issue tracker records
  its issue `merged`, which it does only after GitHub reports the pull
  request merged (ADR 0020), AND GitHub's `merged_by` is this owner's own
  login (the tracker sends `ownerMerged` and the verified `pr` with the
  track change, and the ledger counts only that stored pull request; the owner
  API calls `Ledger::count_trial_merge`, owner-checked). Every Agent shares
  that login, but during the trial the gate never merges, so such a merge is
  the owner's: on GitHub, or by approving the Agent's merge card. Another
  maintainer's merge never counts (GPT review). A merge the gate answered
  itself (an `auto_merge_head_used` claim) never counts, and a replay counts once.
- After N owner merges the trial is over and the gate merges low-risk pull
  requests as before. The owner may end it early through the owner API
  (`{endTrial: "owner/repo"}`, `Ledger::end_watch_trial`, owner-checked);
  GET lists running trials as `trials: [{repo, merged, required}]` for the
  Safeguards card. Nothing an Agent runs or a room member says reaches
  either: both are owner-API writes to the owner's own ledger.
- Migration: a watch without a trial row is past its trial. No row is
  written for existing watches, so the running watches keep merging. A row
  is written only when a repository is new to the work list
  (`set_work`/`set_owner_rules`) or the legacy watch is saved with a
  repository it did not watch before, even saved Off (switching it on later
  is no way round the trial, GPT review); editing, pausing or resuming a
  watch does not start one, and removing and re-adding a watch keeps its count.

## Consequences
- Every Agent shares the owner's GitHub login, so `merged_by` alone cannot
  tell the owner from an Agent; with the gate holding every trial merge, an
  Agent's merge needs the owner's card. An owner's standing "allow" rule for
  merge commands counts as the owner's merge.
- Only pull requests the issue tracker follows count. A maintainer's merges
  of other people's pull requests do not advance the trial.
  TODO(trial): count an owner-approved merge card for untracked PRs.
- TODO(trial-ui): an "End trial" button in the watch settings (Safeguards
  card) posting `{endTrial}`; the API and ledger are in place.
- Accepted residuals (GPT review 3): the owner API trusts the poller's
  `ownerMerged` evidence (an owner-scoped caller could already end the trial
  outright); an owner merge seen while an older automatic-merge claim on the
  same pull request is still unsettled is not counted (fails closed: the
  trial lasts one PR longer).
