"""Unit tests for the runner's pure parts (no network, no live demo).

Run: python3 -m unittest discover -s tools/e2e/tests"""
import sqlite3
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from hx import config, evidence, github, report, scenario  # noqa: E402
from scenarios import issues, manual  # noqa: E402

PUPPET = config.AGENTS["bird-codex"][1]


class MessageShapes(unittest.TestCase):
    def test_mention_routes_by_m_mentions(self):
        c = scenario.mention_content("hi", ["bird-codex"])
        self.assertEqual(c["m.mentions"], {"user_ids": [PUPPET]})
        self.assertIn(f"https://matrix.to/#/{PUPPET}", c["formatted_body"])
        self.assertNotIn("m.relates_to", c)

    def test_thread_follow_up_keeps_mention_and_thread(self):
        c = scenario.mention_content("more", ["bird-codex"], thread_root="$root")
        self.assertEqual(c["m.relates_to"]["rel_type"], "m.thread")
        self.assertEqual(c["m.relates_to"]["event_id"], "$root")
        self.assertEqual(c["m.mentions"]["user_ids"], [PUPPET])

    def test_plain_and_room_mentions_name_no_agent(self):
        self.assertEqual(scenario.plain_content("x")["m.mentions"], {})
        self.assertEqual(scenario.plain_content("x", room_mention=True)["m.mentions"], {"room": True})


class Waiting(unittest.TestCase):
    def test_timeout_is_none_never_success(self):
        t0 = time.time()
        self.assertIsNone(scenario.wait_for(lambda: [], 0.2, 0.05))
        self.assertLess(time.time() - t0, 2)

    def test_returns_first_truthy(self):
        seq = iter([None, [], ["x"]])
        self.assertEqual(scenario.wait_for(lambda: next(seq), 5, 0.01), ["x"])

    def test_substantive_drops_empty_bodies(self):
        evs = [{"content": {"body": ""}}, {"content": {"body": "ok"}}]
        self.assertEqual(scenario.substantive(evs), [evs[1]])


class Blocking(unittest.TestCase):
    def test_missing_agent_blocks_with_owner_named(self):
        ctx = scenario.Ctx(room="!r", setup={"agent:bird-codex": "absent"}, run_id="t")
        res = scenario.blocked("x", ctx, ["bird-codex"])
        self.assertEqual(res.status, scenario.BLOCKED)
        self.assertIn("betty", res.note)

    def test_present_agent_not_blocked(self):
        ctx = scenario.Ctx(room="!r", setup={"agent:bird-codex": "join"}, run_id="t")
        self.assertIsNone(scenario.blocked("x", ctx, ["bird-codex"]))

    def test_injection_is_off_by_default(self):
        ctx = scenario.Ctx(room="!r", setup={}, run_id="t")
        self.assertEqual(manual.s9_failure_injection(ctx, False).status, scenario.OPT_IN)


class GitHubGuards(unittest.TestCase):
    def test_refuses_untagged_issue(self):
        with self.assertRaises(github.GitHubError):
            github.create_issue("no tag", "body")

    def test_refuses_hagency_org(self):
        with self.assertRaises(github.GitHubError):
            github.gh("repo", "view", "hagency-org/hagency-desktop")

    def test_prs_closing_matches_closes_keyword(self):
        rows = [{"number": 5, "title": "t", "body": "Closes #12"},
                {"number": 6, "title": "t", "body": "Closes #120"},
                {"number": 7, "title": "x (#12)", "body": ""}]
        with mock.patch.object(github, "gh", return_value=rows):
            self.assertEqual([p["number"] for p in github.prs_closing(12)], [5])

    def test_branch_naming_issue_is_exact(self):
        names = ["main", "agent/issue-45-e2e-doc", "fix/issue-450-x", "issue45"]
        with mock.patch.object(github, "gh", return_value=names):
            self.assertEqual(github.branches_naming_issue(45), ["agent/issue-45-e2e-doc", "issue45"])

    def test_never_deletes_default_branch(self):
        with mock.patch.object(github, "gh") as gh, self.assertRaises(github.GitHubError):
            github.delete_branch("main")
        gh.assert_not_called()


class IssueLoop(unittest.TestCase):
    def test_describe_names_missing_stages(self):
        st = {"created": 100.0, "handed": 130.0, "ledger_state": "handed"}
        text = issues._describe(7, st)
        self.assertIn("handed+30s", text)
        self.assertIn("missing: pr_opened, reviewed, merged, closed", text)


    def test_out_of_order_stages_fail(self):
        st = {s: 100.0 + i for i, s in enumerate(issues.STAGES)}
        self.assertTrue(issues._complete(st))
        st["reviewed"] = st["merged"] + 60
        self.assertFalse(issues._complete(st))
        self.assertIn("merged after reviewed", " ".join(issues._order_problems(st)))

    def test_cleanup_skips_prs_not_naming_the_issue(self):
        ctx = scenario.Ctx(room="!r", setup={}, run_id="t")
        ctx.loop_states = {12: {"pr": 9, "branch": "feature", "pr_body": "unrelated"}}
        with mock.patch.object(github, "close_pr_if_unmerged") as close:
            issues.cleanup(ctx)
        close.assert_not_called()


class Ledger(unittest.TestCase):
    def test_query_copy_leaves_source_untouched(self):
        with tempfile.TemporaryDirectory() as tmp:
            db = Path(tmp) / "l.sqlite"
            con = sqlite3.connect(db)
            con.execute("create table t(x)")
            con.execute("insert into t values (1)")
            con.commit()
            con.close()
            before = db.stat().st_mtime_ns
            self.assertEqual(evidence.query_copy(db, "select x from t"), [(1,)])
            self.assertEqual(db.stat().st_mtime_ns, before)

    def test_agent_id_from_puppet(self):
        self.assertTrue(evidence.agent_id("bird-claude").startswith("agt_"))


class Reporting(unittest.TestCase):
    def test_report_counts_and_escapes(self):
        ctx = scenario.Ctx(room="!r", setup={"agent:a": "join"}, run_id="t")
        results = [scenario.Result("a|b", scenario.FAIL, 1, ["$e"], "n", "finding"),
                   scenario.Result("c", scenario.BLOCKED, 0, [], "", "setup")]
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(config, "REPORTS_DIR", Path(tmp)):
            from datetime import datetime
            path, summary = report.write_report(datetime(2026, 1, 1), ctx, results, [], [])
            text = path.read_text()
        self.assertIn("1 product finding(s)", summary)
        self.assertIn("a\\|b", text)
        self.assertIn("BLOCKED (setup)", text)


class ExitCodes(unittest.TestCase):
    def test_nothing_verified_is_not_success(self):
        import run
        R = scenario.Result
        self.assertEqual(run.exit_code([]), 2)
        self.assertEqual(run.exit_code([R("a", scenario.BLOCKED)]), 2)
        self.assertEqual(run.exit_code([R("a", scenario.PASS), R("b", scenario.FAIL)]), 1)
        self.assertEqual(run.exit_code([R("a", scenario.PASS), R("b", scenario.OPT_IN)]), 0)


if __name__ == "__main__":
    unittest.main()
