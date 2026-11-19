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

    def test_other_claude_startup_failures_are_not_a_login(self):
        self.assertFalse(provider.is_login_error(
            'Local provider start-up check failed kind="binary" detail="Claude executable not found"'))
        self.assertFalse(provider.is_login_error(
            'Local provider start-up check failed kind="version" detail="Claude Code 2.1.322 or newer is required"'))
        self.assertTrue(provider.is_login_error(
            'Local provider start-up check failed kind="profile" detail="Claude: not logged in"'))

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
        out = provider.signed_out_agents({"anna": False, "betty": False}, {})
        self.assertEqual(set(out), {"anna-claude", "yue-claude", "bird-claude"})
        self.assertTrue(all(provider.SIGNED_OUT_NOTE in v for v in out.values()))

    def test_log_error_blocks_only_that_owners_claude_agents(self):
        out = provider.signed_out_agents({"anna": None, "betty": None}, {"anna": [LOGIN_LINE], "betty": []})
        self.assertEqual(set(out), {"anna-claude", "yue-claude"})

    def test_fresh_cli_logged_in_beats_old_log_errors(self):
        out = provider.signed_out_agents({"anna": True, "betty": None}, {"anna": [LOGIN_LINE], "betty": [LOGIN_LINE]})
        self.assertEqual(set(out), {"bird-claude"})

    def test_all_usable(self):
        self.assertEqual(provider.signed_out_agents({"anna": None}, {"anna": [], "betty": []}), {})

    def test_preflight_reads_logs_and_cli(self):
        with tempfile.TemporaryDirectory() as tmp:
            anna, betty = Path(tmp) / "a.log", Path(tmp) / "b.log"
            anna.write_text(OTHER_LINE + "\n")
            betty.write_text(LOGIN_LINE + "\n")
            with mock.patch.object(provider.config, "DESKTOP_LOGS", {"anna": anna, "betty": betty}), \
                    mock.patch.object(provider, "desktop_config_dir", return_value=None), \
                    mock.patch.object(provider, "claude_cli_logged_in", return_value=None):
                out, log = provider.preflight(NOW)
        self.assertEqual(set(out), {"bird-claude"})
        self.assertTrue(any("bird-claude" in x for x in log))


class ConfigDir(unittest.TestCase):
    """`claude auth status` must check the login the desktop's Agents use."""
    ARGV = "/x/Hagency-fresh-anna.app/Contents/MacOS/rinx --flag CLAUDE_CONFIG_DIR=/argv/not/env"
    PS = ARGV + " HOME=/Users/u CLAUDE_CONFIG_DIR=/Users/u/Library/Hagency Claude/anna HAGENCY_GH_TOKEN_FILE=/t\n"
    TWO_OWNERS = ('if [ "$1" = anna ]; then\n'
                  '  open -n -a "$ROOT/Hagency-fresh-anna.app" --env HOME="$HOME" \\\n'
                  '    --env CLAUDE_CONFIG_DIR="/srv/claude a" --env RUST_LOG=warn\n'
                  'else\n'
                  '  open -n -a "$ROOT/Hagency-fresh-betty.app" --env CLAUDE_CONFIG_DIR=/srv/claude-b\n'
                  'fi\n')

    def test_reads_config_dir_from_the_env_part_only_with_spaces(self):
        self.assertEqual(provider.env_config_dir(self.PS, self.ARGV), "/Users/u/Library/Hagency Claude/anna")
        self.assertIsNone(provider.env_config_dir(self.ARGV + " HOME=/Users/u PATH=/bin", self.ARGV))
        self.assertIsNone(provider.env_config_dir(self.ARGV + " MY_CLAUDE_CONFIG_DIR=/nope", self.ARGV))
        self.assertEqual(provider.env_config_dir("other HOME=/h", self.ARGV), provider.UNKNOWN_DIR)

    def test_script_value_comes_from_that_owners_launch_block(self):
        self.assertEqual(provider.script_config_dir(self.TWO_OWNERS, "anna"), "/srv/claude a")
        self.assertEqual(provider.script_config_dir(self.TWO_OWNERS, "betty"), "/srv/claude-b")

    def test_script_value_it_cannot_attribute_or_resolve_is_unknown(self):
        generic = 'open -n -a "$APP" --env HOME="$HOME" \\\n  --env CLAUDE_CONFIG_DIR=/srv/shared\n'
        self.assertEqual(provider.script_config_dir(generic, "anna"), provider.UNKNOWN_DIR)
        var = 'open -a Hagency-fresh-anna.app --env CLAUDE_CONFIG_DIR="$AGENT_DIR"'
        self.assertEqual(provider.script_config_dir(var, "anna"), provider.UNKNOWN_DIR)
        self.assertIsNone(provider.script_config_dir('open -a "$APP" --env HOME="$HOME"', "anna"))

    def _desktop_dir(self, ps_env, script, argv=ARGV):
        with tempfile.TemporaryDirectory() as tmp:
            pid, sh = Path(tmp) / "desktop.pid", Path(tmp) / "up.sh"
            pid.write_text("70973\n")
            sh.write_text(script)
            outs = {"-wwE": ps_env, "-ww": argv + "\n"}
            with mock.patch.object(provider.config, "DESKTOP_PIDS", {"anna": pid}), \
                    mock.patch.object(provider.config, "DEMO_UP_SCRIPT", sh), \
                    mock.patch.object(provider.subprocess, "run",
                                      side_effect=lambda cmd, **_: mock.Mock(stdout=outs[cmd[1]])) as run:
                return provider.desktop_config_dir("anna"), run

    def test_process_env_wins_then_script_then_none(self):
        got, run = self._desktop_dir(self.PS, self.TWO_OWNERS)
        self.assertEqual(got, "/Users/u/Library/Hagency Claude/anna")
        self.assertIn(["ps", "-wwE", "-p", "70973", "-o", "command="], [c[0][0] for c in run.call_args_list])
        self.assertIsNone(self._desktop_dir(self.ARGV + " HOME=/h\n", self.TWO_OWNERS)[0])
        self.assertEqual(self._desktop_dir("garbled", self.TWO_OWNERS)[0], "/srv/claude a")
        self.assertIsNone(self._desktop_dir("garbled", 'open -a "$APP" --env HOME="$HOME"')[0])

    def test_unknown_dir_skips_the_cli_probe(self):
        probe = mock.Mock(return_value=False)
        self.assertEqual(provider.owner_cli_status({"anna": provider.UNKNOWN_DIR}, probe), {"anna": None})
        probe.assert_not_called()

    def _auth_env(self, config_dir, inherited):
        ran = mock.Mock(stdout='{"loggedIn": true}')
        with mock.patch.dict(provider.os.environ, inherited, clear=False), \
                mock.patch.object(provider.subprocess, "run", return_value=ran) as run:
            self.assertIs(provider.claude_cli_logged_in(config_dir), True)
        return run.call_args.kwargs["env"]

    def test_auth_status_runs_under_the_desktops_config_dir(self):
        env = self._auth_env("/Users/u/.hagency-claude/anna", {"CLAUDE_CONFIG_DIR": "/elsewhere"})
        self.assertEqual(env["CLAUDE_CONFIG_DIR"], "/Users/u/.hagency-claude/anna")

    def test_auth_status_unsets_config_dir_when_the_desktop_has_none(self):
        env = self._auth_env(None, {"CLAUDE_CONFIG_DIR": "/elsewhere"})
        self.assertNotIn("CLAUDE_CONFIG_DIR", env)
        self.assertIn("PATH", env)

    def test_one_probe_per_distinct_dir_and_per_owner_result(self):
        probe = mock.Mock(side_effect=lambda d: d is None)
        got = provider.owner_cli_status({"anna": None, "betty": "/srv/b"}, probe)
        self.assertEqual(got, {"anna": True, "betty": False})
        self.assertEqual(probe.call_count, 2)
        out = provider.signed_out_agents(got, {}, {"anna": None, "betty": "/srv/b"})
        self.assertEqual(set(out), {"bird-claude"})
        self.assertIn("CLAUDE_CONFIG_DIR=/srv/b", out["bird-claude"])


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
        ctx = _ctx(provider.signed_out_agents({"anna": False, "betty": False}, {}))
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

    def test_no_answer_plus_another_failure_stays_fail(self):
        note = ("no answer quoting the title in 300s; bird-claude said: '(nothing)'. "
                "approval(s) raised for a read-only query: ['$abc']. ")
        with tempfile.TemporaryDirectory() as tmp:
            offsets = self._offsets(tmp, "", LOGIN_LINE + "\n")
            r = provider.reclassify_silence(self._fail(note), offsets)
        self.assertEqual((r.status, r.kind), (FAIL, "finding"))

    def test_every_item_silent_and_signed_out_is_blocked(self):
        note = ("s2a: no answer with T from anna-claude in 240s (asked $a). "
                "s2b: no answer with T from bird-claude in 240s (asked $b). ")
        with tempfile.TemporaryDirectory() as tmp:
            offsets = self._offsets(tmp, LOGIN_LINE + "\n", LOGIN_LINE + "\n")
            self.assertEqual(provider.reclassify_silence(self._fail(note), offsets).status, BLOCKED)
            offsets = self._offsets(tmp, LOGIN_LINE + "\n", "")
            self.assertEqual(provider.reclassify_silence(self._fail(note), offsets).status, FAIL)

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
