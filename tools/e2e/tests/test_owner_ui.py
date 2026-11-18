"""Owner steps against a fake window (no live demo)."""
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from hx import bridge, owner_ui  # noqa: E402

NOTICE = "body.room.d6fc.shared_permission_notice"


def n(nid, path, name="", actions="", bounds=(0, 0, 100, 20), role="button"):
    return {"id": nid, "path": path, "name": name, "text": name, "actions": actions,
            "bounds": list(bounds), "role": role, "window": "1:0"}


ROOM_ROW = n("r1", "rooms_list.list.FullPreview.top.room_name", "e2e room", role="static_text")


class FakeBridge:
    """Answers every query with the current screen; records presses/clicks."""

    def __init__(self, screen):
        self.owner, self.screen, self.done = "betty", screen, []

    def nodes(self, query):
        return list(self.screen)

    def press(self, node, settle=1.0):
        self.done.append(("press", node["id"]))

    def click_at(self, node, settle=1.0):
        self.done.append(("click", node["id"]))


class Matching(unittest.TestCase):
    def test_same_card_by_widget_path(self):
        deny = n("d", f"{NOTICE}.actions.deny", "Deny", "focus,press")
        details = n("t", f"{NOTICE}.request_details.details", "git push origin e2e-1-s6")
        chat = n("m", "body.room.timeline.message.text", "please create branch e2e-1-s6")
        self.assertTrue(bridge.same_card(details, deny, 2))
        self.assertFalse(bridge.same_card(chat, deny, 2))

    def test_deny_only_on_the_card_naming_the_token(self):
        ours = [n("t1", f"{NOTICE}.request_details.details", "git push origin e2e-1-s6"),
                n("d1", f"{NOTICE}.actions.deny", "Deny", "focus,press")]
        other = [n("t2", "inbox.row0.text", "ls -a ./sandbox"),
                 n("d2", "inbox.row0.buttons.deny", "Deny", "focus,press")]
        chat = [n("m", "timeline.message.text", "please create branch e2e-1-s6")]
        self.assertEqual([d["id"] for d in owner_ui.card_deny(ours + other + chat, "e2e-1-s6")], ["d1"])
        self.assertEqual(owner_ui.card_deny(other + chat, "e2e-1-s6"), [])

    def test_prompt_button_needs_the_e2e_room_in_its_own_prompt(self):
        mine = [n("p", "prompt.body.text", "Bring bird-codex into e2e room?"),
                n("b", "prompt.buttons.bring", "Bring", "press")]
        elsewhere = [n("p2", "prompt2.body.text", "Bring bird-codex into dev room?"),
                     n("b2", "prompt2.buttons.bring", "Bring", "press"), ROOM_ROW]
        self.assertEqual([x["id"] for x in owner_ui.prompt_buttons(mine + elsewhere)], ["b"])

    def test_there_is_no_approve_path(self):
        src = Path(owner_ui.__file__).read_text()
        self.assertNotIn("once", src.replace("answered once", ""))
        self.assertFalse(hasattr(owner_ui, "decide_card"))


class Flows(unittest.TestCase):
    def setUp(self):
        self.statuses = []
        for target, value in ((owner_ui, "_status"),):
            p = mock.patch.object(target, value, side_effect=self._next_status)
            p.start()
            self.addCleanup(p.stop)
        p = mock.patch("time.sleep")
        p.start()
        self.addCleanup(p.stop)

    def _next_status(self, _room):
        return self.statuses.pop(0) if len(self.statuses) > 1 else self.statuses[0]

    def test_join_skipped_when_already_joined(self):
        self.statuses = [{"owner:betty": "join"}]
        fake, log = FakeBridge([]), []
        owner_ui.join_room(fake, "!r", log)
        self.assertEqual(fake.done, [])

    def test_join_opens_room_then_presses_join_and_waits_for_matrix(self):
        self.statuses = [{"owner:betty": "invite"}] * 2 + [{"owner:betty": "join"}]
        fake = FakeBridge([ROOM_ROW, n("j", "invite_screen.buttons.accept_button", "Join Room", "press")])
        owner_ui.join_room(fake, "!r", [])
        self.assertEqual(fake.done, [("click", "r1"), ("press", "j")])

    def test_missing_join_button_is_named(self):
        self.statuses = [{"owner:betty": "invite"}]
        fake = FakeBridge([ROOM_ROW])
        with mock.patch.object(owner_ui, "wait_nodes", side_effect=[[ROOM_ROW], None]):
            with self.assertRaisesRegex(bridge.StepNotFound, "Join Room"):
                owner_ui.join_room(fake, "!r", [])

    def test_add_agent_walks_plus_picker_invite(self):
        self.statuses = [{"agent:bird-codex": "absent"}] * 2 + [{"agent:bird-codex": "join"}]
        fake = FakeBridge([ROOM_ROW, n("plus", "search_row.new_or_join_button.icon", role="image"),
                           n("add", "menu.plus_add_agent", "Add agent to this room", "press"),
                           n("pp", "invite_modal.people_section.people_list.Person.person_name", "bird-codex",
                             role="static_text"),
                           n("ok", "invite_modal.buttons_view.confirm_button", "Invite", "press")])
        with mock.patch.object(owner_ui, "_allow_if_private"):
            owner_ui.add_agent(fake, "!r", "bird-codex", [])
        self.assertEqual(fake.done, [("click", "r1"), ("click", "plus"), ("press", "add"), ("click", "pp"),
                                     ("press", "ok")])

    def test_add_agent_skips_present_agent(self):
        self.statuses = [{"agent:bird-codex": "join"}]
        fake = FakeBridge([])
        owner_ui.add_agent(fake, "!r", "bird-codex", [])
        self.assertEqual(fake.done, [])

    def test_deny_presses_only_our_card(self):
        fake = FakeBridge([ROOM_ROW, n("t", f"{NOTICE}.request_details.details", "git push e2e-1-s6"),
                           n("d", f"{NOTICE}.actions.deny", "Deny", "press"),
                           n("o", f"{NOTICE}.actions.once", "Approve once", "press")])
        with mock.patch.object(owner_ui, "Bridge", return_value=fake):
            owner_ui.deny_card("betty", "e2e-1-s6")
        self.assertEqual(fake.done, [("click", "r1"), ("press", "d")])


if __name__ == "__main__":
    unittest.main()
