# Hagency local-demo e2e scenarios

Black-box scenario runner for the local federated demo (alpha 8090 /
localhost:8450, beta 8092 / localhost:8452, Anna's and Betty's desktop
windows). It acts as throwaway Matrix users, then checks room events from both
servers, the owners' ledgers (read from copies), GitHub, and logs.

    python3 tools/e2e/run.py                 # everything (≈30 min, scenario 7 dominates)
    python3 tools/e2e/run.py --only 1,2,3    # a subset
    python3 tools/e2e/run.py --inject pause-betty   # also run the opt-in injection
    python3 -m unittest discover -s tools/e2e/tests

Output: `tools/e2e/reports/<timestamp>.md` plus a one-line summary; exit 1 on
any FAIL. Python 3.10+ stdlib only, plus the logged-in `gh` CLI.

First run: see [SETUP.md](SETUP.md) — one Join click per owner window.

## Scenarios

| # | What | Pass condition |
|---|---|---|
| 1 | zz-anna @mentions anna-claude | answer with the token, latency recorded, visible on both servers |
| 2 | beta user asks alpha agent, alpha user asks beta agent | both answered, both servers see the answers |
| 3 | single-agent question, then thread follow-up with the mention the composer auto-adds; then a no-mention thread reply | follow-up answered in the thread; no-mention reply ignored for 75 s |
| 4 | `@room` announcement | no agent message for 75 s |
| 5 | non-owner read-only repo question to bird-claude | answered; no new approval in Betty's ledger |
| 6 | non-owner asks bird-claude to push a branch | approval appears in Betty's ledger; branch not pushed |
| 7 | two `[e2e]` issues in the sandbox repo at once | each: hand-off (ledger `issue_track`) → PR "Closes #N" → review → merge → issue closed, with stage timings |
| 8 | zz-anna asks bird-codex to review the PR from 7 (or a runner PR) | bird's card for the review post appears in Betty's window; the runner approves it once; within 90 s a review with `Hagency verdict: APPROVE <head>` (or CHANGES), the room line "Hagency posted this Agent's review verdict on …" (turn already ended) or bird-codex's reply linking the review, and no second card for the PR |
| 9 | Betty's window SIGSTOPped while asked, then SIGCONT | answered after resume (opt-in; other injections listed) |
| 10 | agent add/remove/Leave→Message | MANUAL: owner-only, in-process (see SETUP.md) |

Results: PASS; FAIL (finding) = product behaviour; FAIL (runner) = harness
error with traceback; BLOCKED = precondition missing; never counted as PASS
(timeouts and empty answers are FAIL, R14).

Provider preflight (`hx/provider.py`): before any scenario the runner reads
`claude auth status` (`loggedIn` only) and the last 5 min of each owner's
desktop log for "dedicated owner Claude login required". A signed-out Claude
Agent's scenarios (1, 2, 5, 5b, 6, 7, 9) are BLOCKED "agent signed out (Claude
login)" without running; Codex-only ones (3, 4, 8) still run. A scenario that
FAILs with no answer from a Claude Agent while its owner's log showed that
error during the scenario is reported BLOCKED too.

## Safety

- Writes only: the e2e room, `[e2e]`-titled issues and `e2e-*` branches in
  `csheargm/hagency-demo-sandbox`. Refuses `hagency-org` and port 8088.
- Never writes the dev room; scenario 7's agent reports there by its own
  configuration (bird-claude's issue watch), which the runner does not change.
- Owner ledgers are copied before reading. Tokens live in
  `hagency-server/.run/e2e/tokens/` (0600) and are never printed.
- Cleanup: unmerged runner PRs closed with their branch deleted; merged PRs
  stay (their head branch is deleted); runner issues closed.

## Gaps

- Agent binding/removal has no external owner endpoint (SETUP.md).
- Scenario 3 simulates the composer's auto-added mention; the composer
  itself is not exercised (needs a UI driver).
- Approval decisions are driven in the owner's window: Deny on scenarios 5b/6
  cards, Approve once only on scenario 8's review-post card.
