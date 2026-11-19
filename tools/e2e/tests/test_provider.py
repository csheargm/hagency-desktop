"""Signed-out provider classification (hx/provider.py) on sample log lines.

Run: python3 -m unittest discover -s tools/e2e/tests"""
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from hx import provider, scenario  # noqa: E402
from hx.scenario import BLOCKED, FAIL, PASS, Ctx, Result  # noqa: E402
from scenarios import chat, issues  # noqa: E402

# Verbatim shape of fresh-anna/desktop.log at 02:01 local (ANSI colour codes included).
LOGIN_LINE = ('\x1b[2m2026-11-19T11:58:52.520692Z\x1b[0m \x1b[33m WARN\x1b[0m '
              '\x1b[2mhagency::console::owned_runtime\x1b[0m\x1b[2m:\x1b[0m Local provider start-up check failed '
              '\x1b[3mkind\x1b[0m\x1b[2m=\x1b[0m"profile" \x1b[3mdetail\x1b[0m\x1b[2m=\x1b[0m'
              '"dedicated owner Claude login required" \x1b[3mevent\x1b[0m\x1b[2m=\x1b[0m"none"')
OTHER_LINE = ('\x1b[2m2026-11-19T11:59:59.888154Z\x1b[0m \x1b[33m WARN\x1b[0m '
              'hagency::console::native::alert_observer: Resource alert observation deferred '
              'code=device_setup_required')
NOW = datetime(2026, 11, 19, 12, 0, 0, tzinfo=timezone.utc)


class LogLines(unittest.TestCase):
    def test_login_error_line_is_recognised_through_ansi(self):
        self.assertTrue(provider.is_login_error(LOGIN_LINE))
        self.assertFalse(provider.is_login_error(OTHER_LINE))

    def test_startup_failure_naming_another_provider_is_not_a_claude_login(self):
        self.assertFalse(provider.is_login_error("Local provider start-up check failed detail=\"codex missing\""))

    def test_line_time_parses_utc_stamp(self):
        self.assertEqual(provider.line_time(LOGIN_LINE), datetime(2026, 11, 19, 11, 58, 52, tzinfo=timezone.utc))
        self.assertIsNone(provider.line_time("no timestamp here"))

    def test_only_recent_errors_count(self):
        old = LOGIN_LINE.replace("2026-11-19T11:58:52", "2026-11-19T11:40:00")
        hits = provider.recent_login_errors("\n".join([old, OTHER_LINE, LOGIN_LINE]), NOW)
        self.assertEqual(len(hits), 1)
        self.assertNotIn("\x1b", hits[0])
        self.assertEqual(provider.recent_login_errors(old, NOW), [])


class AuthStatus(unittest.TestCase):
    def test_reads_logged_in_only(self):
        self.assertIs(provider.parse_auth_status('{"loggedIn": false, "authMethod": "none"}'), False)
        self.assertIs(provider.parse_auth_status('{"loggedIn": true}'), True)

    def test_unreadable_status_is_unknown(self):
        self.assertIsNone(provider.parse_auth_status("not json"))
        self.assertIsNone(provider.parse_auth_status("[]"))
        self.assertIsNone(provider.parse_auth_status('{"loggedIn": "yes"}'))


class SignedOutAgents(unittest.TestCase):
    def test_cli_signed_out_blocks_every_claude_agent_not_codex(self):
        out = provider.signed_out_agents(False, {})
        self.assertEqual(set(out), {"anna-claude", "yue-claude", "bird-claude"})
        self.assertTrue(all(provider.SIGNED_OUT_NOTE in v for v in out.values()))

    def test_log_error_blocks_only_that_owners_claude_agents(self):
        out = provider.signed_out_agents(True, {"anna": [LOGIN_LINE], "betty": []})
        self.assertEqual(set(out), {"anna-claude", "yue-claude"})

    def test_all_usable(self):
        self.assertEqual(provider.signed_out_agents(None, {"anna": [], "betty": []}), {})

    def test_preflight_reads_logs_and_cli(self):
        with tempfile.TemporaryDirectory() as tmp:
            anna, betty = Path(tmp) / "a.log", Path(tmp) / "b.log"
            anna.write_text(OTHER_LINE + "\n")
            betty.write_text(LOGIN_LINE + "\n")
            with mock.patch.object(provider.config, "DESKTOP_LOGS", {"anna": anna, "betty": betty}), \
                    mock.patch.object(provider, "claude_cli_logged_in", return_value=True):
                out, log = provider.preflight(NOW)
        self.assertEqual(set(out), {"bird-claude"})
        self.assertTrue(any("bird-claude" in x for x in log))


def _ctx(signed_out=None):
    agents = {f"agent:{a}": "join" for a in provider.config.AGENTS}
    return Ctx(room="!r", setup=agents, run_id="x", signed_out=signed_out or {})


class ScenarioGate(unittest.TestCase):
    def test_claude_scenario_blocked_before_running(self):
        ctx = _ctx({"anna-claude": "agent signed out (Claude login): x"})
        with mock.patch.object(chat, "_check_answer", side_effect=AssertionError("must not run")):
            r = chat.s1_mention_answer(ctx)
        self.assertEqual(r.status, BLOCKED)
        self.assertIn(provider.SIGNED_OUT_NOTE, r.note)

    def test_codex_scenario_not_blocked_by_claude_sign_out(self):
        ctx = _ctx(provider.signed_out_agents(False, {}))
        self.assertIsNone(scenario.blocked("3", ctx, ["bird-codex"]))

    def test_issue_loop_blocked_when_loop_agent_signed_out(self):
        ctx = _ctx({issues.LOOP_AGENT: "agent signed out (Claude login): x"})
        with mock.patch.object(issues, "_open_issue", side_effect=AssertionError("must not run")):
            self.assertEqual(issues.s7_issue_loop(ctx).status, BLOCKED)


class TimeoutReclassification(unittest.TestCase):
    def _offsets(self, tmp, anna_text, betty_text):
        anna, betty = Path(tmp) / "a.log", Path(tmp) / "b.log"
        anna.write_text("before\n")
        betty.write_text("before\n")
        offsets = {"desktop:anna": (anna, anna.stat().st_size), "desktop:betty": (betty, betty.stat().st_size)}
        with open(anna, "a") as f:
            f.write(anna_text)
        with open(betty, "a") as f:
            f.write(betty_text)
        return offsets

    def _fail(self, note):
        return Result("5", FAIL, 300, [], note, kind="finding")

    def test_silent_claude_agent_with_login_error_in_window_is_blocked(self):
        with tempfile.TemporaryDirectory() as tmp:
            offsets = self._offsets(tmp, "", LOGIN_LINE + "\n")
            r = provider.reclassify_silence(self._fail("no answer in 300s; bird-claude said: '(nothing)'."), offsets)
        self.assertEqual((r.status, r.kind), (BLOCKED, "setup"))
        self.assertIn(provider.SIGNED_OUT_NOTE, r.note)
        self.assertNotIn("\x1b", r.note)

    def test_s1_style_no_answer_note_is_blocked(self):
        with tempfile.TemporaryDirectory() as tmp:
            offsets = self._offsets(tmp, LOGIN_LINE + "\n", "")
            r = provider.reclassify_silence(self._fail("s1: no answer with T from anna-claude in 240s"), offsets)
        self.assertEqual(r.status, BLOCKED)

    def test_error_only_before_the_window_stays_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            offsets = self._offsets(tmp, "", OTHER_LINE + "\n")
            r = provider.reclassify_silence(self._fail("bird-claude said: '(nothing)'"), offsets)
        self.assertEqual(r.status, FAIL)

    def test_other_owners_error_does_not_excuse_this_agent(self):
        with tempfile.TemporaryDirectory() as tmp:
            offsets = self._offsets(tmp, LOGIN_LINE + "\n", "")
            r = provider.reclassify_silence(self._fail("bird-claude said: '(nothing)'"), offsets)
        self.assertEqual(r.status, FAIL)

    def test_codex_silence_stays_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            offsets = self._offsets(tmp, LOGIN_LINE + "\n", LOGIN_LINE + "\n")
            r = provider.reclassify_silence(self._fail("bird-codex said: '(nothing)'"), offsets)
        self.assertEqual(r.status, FAIL)

    def test_wrong_answer_and_pass_untouched(self):
        with tempfile.TemporaryDirectory() as tmp:
            offsets = self._offsets(tmp, LOGIN_LINE + "\n", LOGIN_LINE + "\n")
            wrong = provider.reclassify_silence(self._fail("bird-claude answered the wrong title"), offsets)
            ok = provider.reclassify_silence(Result("1", PASS), offsets)
        self.assertEqual((wrong.status, ok.status), (FAIL, PASS))


if __name__ == "__main__":
    unittest.main()
