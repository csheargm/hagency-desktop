"""Bridge-driven owner steps against a fake bridge (no live window)."""
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from hx import bridge, owner_ui  # noqa: E402


def w(i, t="", r=(0, 0, 100, 20), ty="Button"):
    return {"i": i, "t": t, "r": list(r), "ty": ty}


class FakeBridge:
    """Serves scripted snapshots; records clicks."""

    def __init__(self, screens):
        self.owner, self.screens, self.clicks, self.clicked = "betty", list(screens), [], []

    def snap(self, query="", every=False):
        return self.screens[0] if len(self.screens) == 1 else self.screens.pop(0)

    def click(self, widget, settle=1.0):
        self.clicks.append(widget["i"] or widget["t"])
        self.clicked.append(widget)


class BridgeBasics(unittest.TestCase):
    def test_closed_port_says_bridge_not_up(self):
        with mock.patch.object(bridge, "port_open", return_value=False):
            with self.assertRaisesRegex(bridge.BridgeDown, "bridge not up yet"):
                bridge.Bridge("betty")

    def test_never_closes_a_demo_window(self):
        with mock.patch.object(bridge, "port_open", return_value=True):
            b = bridge.Bridge("anna")
        for route in ("/gq", "/quit", "/close"):
            with self.assertRaises(ValueError):
                b.get(route)

    def test_nearest_below_picks_the_same_card(self):
        anchor = w("details", "git push e2e-1-s6", (0, 100, 300, 20))
        mine = w("deny", "Deny", (0, 160, 100, 20))
        other = w("deny", "Deny", (0, 600, 100, 20))
        above = w("deny", "Deny", (0, 20, 100, 20))
        self.assertIs(bridge.nearest_below(anchor, [other, above, mine]), mine)


class Anchors(unittest.TestCase):
    def test_card_anchor_ignores_the_runners_own_chat_message(self):
        ws = [w("message", "please create branch e2e-1-s6"), w("details", "git push origin e2e-1-s6")]
        self.assertEqual([a["i"] for a in owner_ui.card_anchors(ws, "e2e-1-s6")], ["details"])

    def test_prompt_anchor_is_the_sentence_not_the_room_row(self):
        ws = [w("room_name", "e2e room"), w("text", "Bring bird-claude into e2e room?")]
        self.assertEqual([a["i"] for a in owner_ui.prompt_anchors(ws)], ["text"])

    def test_there_is_no_approve_path(self):
        self.assertFalse(hasattr(owner_ui, "decide_card"))
        self.assertNotIn('"once"', Path(owner_ui.__file__).read_text())

    def test_room_row_must_match_exactly(self):
        ws = [w("row", "old e2e room"), w("msg", "see e2e room later"), w("row2", "e2e room")]
        self.assertEqual([x["i"] for x in bridge.exact(ws, "e2e room")], ["row2"])

    def test_deny_never_crosses_into_the_next_card(self):
        anchor = w("details", "git push e2e-1-s6", (0, 100, 300, 20))
        next_title = w("title", "bird-claude wants to use Bash", (0, 140, 300, 20))
        next_deny = w("deny", "Deny", (0, 200, 100, 20))
        self.assertIsNone(bridge.nearest_below(anchor, [next_deny], (next_title,)))

    def test_moved_widget_is_not_clicked(self):
        with mock.patch.object(bridge, "port_open", return_value=True):
            b = bridge.Bridge("betty")
        target = w("deny", "Deny", (0, 100, 100, 20))
        with mock.patch.object(b, "snap", return_value=[w("deny", "Deny", (0, 300, 100, 20))]), \
                mock.patch.object(b, "get") as get, self.assertRaises(bridge.StepNotFound):
            b.click(target)
        get.assert_not_called()

    def test_bridge_errors_become_bridge_down(self):
        with mock.patch.object(bridge, "port_open", return_value=True):
            b = bridge.Bridge("betty")
        with mock.patch("urllib.request.urlopen", side_effect=OSError("reset")), \
                self.assertRaises(bridge.BridgeDown):
            b.snap()


class Flows(unittest.TestCase):
    def setUp(self):
        self.statuses = []
        patcher = mock.patch.object(owner_ui, "_status", side_effect=self._next_status)
        patcher.start()
        self.addCleanup(patcher.stop)
        sleep = mock.patch("time.sleep")
        sleep.start()
        self.addCleanup(sleep.stop)

    def _next_status(self, _room):
        return self.statuses.pop(0) if len(self.statuses) > 1 else self.statuses[0]

    def test_join_is_skipped_when_already_joined(self):
        self.statuses = [{"owner:betty": "join"}]
        fake, log = FakeBridge([[]]), []
        owner_ui.join_room(fake, "!r", log)
        self.assertEqual(fake.clicks, [])
        self.assertIn("already joined", log[0])

    def test_join_clicks_room_then_join_room_and_waits_for_matrix(self):
        self.statuses = [{"owner:betty": "invite"}, {"owner:betty": "invite"}, {"owner:betty": "join"}]
        screen = [w("room_row", "e2e room"), w("accept_button", "Join Room")]
        fake, log = FakeBridge([screen]), []
        owner_ui.join_room(fake, "!r", log)
        self.assertEqual(fake.clicks, ["room_row", "accept_button"])

    def test_missing_join_button_is_a_named_failure(self):
        self.statuses = [{"owner:betty": "invite"}]
        fake = FakeBridge([[w("room_row", "e2e room")]])
        with mock.patch.object(owner_ui, "wait_widget", side_effect=[[w("room_row", "e2e room")], None]):
            with self.assertRaisesRegex(bridge.StepNotFound, "Join Room"):
                owner_ui.join_room(fake, "!r", [])

    def test_add_agent_skips_present_agent(self):
        self.statuses = [{"agent:bird-codex": "join"}]
        fake, log = FakeBridge([[]]), []
        owner_ui.add_agent(fake, "!r", "bird-codex", log)
        self.assertEqual(fake.clicks, [])

    def test_add_agent_walks_plus_menu_picker_and_invite(self):
        self.statuses = [{"agent:bird-codex": "absent"}] * 2 + [{"agent:bird-codex": "join"}]
        screen = [w("room_row", "e2e room"), w("new_or_join_button"), w("plus_add_agent", "Add agent to this room"),
                  w("person_name", "bird-codex", ty="Label"), w("confirm_button", "Invite")]
        fake, log = FakeBridge([screen]), []
        with mock.patch.object(owner_ui, "_allow_if_private"):
            owner_ui.add_agent(fake, "!r", "bird-codex", log)
        self.assertEqual(fake.clicks, ["room_row", "new_or_join_button", "plus_add_agent", "person_name",
                                       "confirm_button"])

    def test_deny_clicks_only_the_card_naming_the_token(self):
        screen = [w("room_row", "e2e room"), w("message", "create branch e2e-1-s6", (0, 50, 300, 20)),
                  w("deny", "Deny", (0, 80, 100, 20)),  # under the chat message, not ours
                  w("details", "git push origin e2e-1-s6", (0, 300, 300, 20)),
                  w("deny", "Deny", (0, 360, 100, 20))]
        fake = FakeBridge([screen])
        with mock.patch.object(owner_ui, "Bridge", return_value=fake):
            owner_ui.deny_card("betty", "e2e-1-s6")
        self.assertEqual(len(fake.clicks), 2)  # room row, then the card's own Deny
        self.assertEqual(fake.clicks[-1], "deny")
        self.assertEqual(fake.clicked[-1]["r"][1], 360, "the Deny under the card, not under the chat message")


if __name__ == "__main__":
    unittest.main()
