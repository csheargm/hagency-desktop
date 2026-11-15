//! The agent auto-merge and review instructions, shared by every runtime that
//! serves a room (Claude's AGENT_TOOLS_NOTE and Codex's developer instructions),
//! so the text the merge gate expects lives in exactly one place.

/// The literal behind [`AGENT_MERGE_NOTE`], usable inside `concat!`.
macro_rules! agent_merge_note {
    () => {
        "Agent auto-merge (when the hand-off says the repository has an agent merge policy, or after opening a pull request you find .hagency/merge-policy.yml on its base branch: git show origin/<base>:.hagency/merge-policy.yml): when your pull request is ready, mark it ready (gh pr ready <N> --repo <owner/repo>, as its own command) and say in the room \"@<reviewer agent> please review PR <N>\" if a reviewer Agent is in the room; once it has posted its verdict, read the head commit (gh pr view <N> --repo <owner/repo> --json headRefOid -q .headRefOid) and run exactly `gh pr merge <N> --squash --match-head-commit <sha> --repo <owner/repo>` as its own command, never chained with && or ;. An approval only counts for the head commit it reviewed, so after a new push ask for a fresh review. Hagency allows that merge without a card only when the policy and another Agent's approval qualify it; otherwise the card asks the owner, which is expected, not an error. Never approve your own pull request. \
When asked to review a pull request: read it (gh pr view <N>, gh pr diff <N>), then post your verdict with exactly one gh pr review <N> command: if it is correct, small and in scope, use --comment -b \"Hagency verdict: APPROVE - <one-line reason>\" (all Agents share one GitHub account, which cannot formally approve its own pull request); otherwise use --comment -b \"Hagency verdict: CHANGES - <what is wrong>\" and never merge."
    };
}

/// How an Agent asks for review, posts a verdict and merges a pull request.
pub const AGENT_MERGE_NOTE: &str = agent_merge_note!();
