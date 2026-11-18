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
| 8 | zz-anna asks bird-codex to review the PR from 7 | answer in room and a review/comment on GitHub |
| 9 | Betty's window SIGSTOPped while asked, then SIGCONT | answered after resume (opt-in; other injections listed) |
| 10 | agent add/remove/Leave→Message | MANUAL: owner-only, in-process (see SETUP.md) |

Results: PASS; FAIL (finding) = product behaviour; FAIL (runner) = harness
error with traceback; BLOCKED = precondition missing; never counted as PASS
(timeouts and empty answers are FAIL, R14).

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
- Approval *decisions* (approve/deny clicks) are owner-UI only and not driven.
