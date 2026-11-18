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


ROOM_ROW = n("r1", "dock.rooms_list.list.FullPreview.top.room_name", "e2e room", role="static_text")


class FakeBridge:
    """Answers every query with the current screen; records presses/clicks."""

    def __init__(self, screen):
        self.owner, self.screen, self.done = "betty", screen, []

    def snapshot(self):
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

    def test_inbox_row_must_be_in_the_e2e_room(self):
        inbox = "overlay.owner_approvals_inbox.card.list"
        dev = [n("t1", f"{inbox}.row0.details_scroll.details", "gh issue view 62 -R x/y"),
               n("p1", f"{inbox}.row0.place", "In dev room · bird-claude"),
               n("d1", f"{inbox}.row0.buttons.deny", "Deny", "press")]
        e2e = [n("t2", f"{inbox}.row1.details_scroll.details", "gh issue view 62 -R x/y"),
               n("p2", f"{inbox}.row1.place", "In e2e room · bird-claude"),
               n("d2", f"{inbox}.row1.buttons.deny", "Deny", "press")]
        self.assertEqual([d["id"] for d in owner_ui.card_deny(dev + e2e, "issue view 62")], ["d2"])

    def test_same_card_in_notice_and_inbox_counts_once(self):
        inbox = "overlay.owner_approvals_inbox.card.list"
        both = [n("t", f"{NOTICE}.request_details.details", "gh api -X POST ... e2e-1-s6"),
                n("d", f"{NOTICE}.actions.deny", "Deny", "press"),
                n("t2", f"{inbox}.row0.details_scroll.details", "gh api -X POST ... e2e-1-s6"),
                n("p2", f"{inbox}.row0.place", "In e2e room · bird-claude"),
                n("d2", f"{inbox}.row0.buttons.deny", "Deny", "press")]
        self.assertEqual([d["id"] for d in owner_ui.one_card(owner_ui.card_deny(both, "e2e-1-s6"))], ["d"])

    def test_expired_card_has_no_deny_so_it_is_not_pending(self):
        inbox = "overlay.owner_approvals_inbox.card.list"
        expired = [n("t", f"{inbox}.row0.details_scroll.details", "gh repo clone e2e-1-s6"),
                   n("p", f"{inbox}.row0.place", "In e2e room · bird-claude"),
                   n("x", f"{inbox}.row0.buttons.dismiss", "Dismiss", "press")]
        self.assertEqual(owner_ui.card_deny(expired, "e2e-1-s6"), [])

    def test_prompt_button_needs_the_e2e_room_in_its_own_prompt(self):
        mine = [n("p", "prompt.body.text", "Bring bird-codex into e2e room?"),
                n("b", "prompt.buttons.bring", "Bring", "press")]
        elsewhere = [n("p2", "prompt2.body.text", "Bring bird-codex into dev room?"),
                     n("b2", "prompt2.buttons.bring", "Bring", "press"), ROOM_ROW]
        self.assertEqual([b["id"] for b, _ in owner_ui.prompt_buttons(mine + elsewhere)], ["b"])

    def test_room_row_must_be_unique_and_in_the_room_list(self):
        header = n("h", "room_screen.header.room_name", "e2e room", role="static_text")
        self.assertEqual([x["id"] for x in owner_ui.room_rows([ROOM_ROW, header], "e2e room")], ["r1"])
        fake = FakeBridge([ROOM_ROW, dict(ROOM_ROW, id="r2")])
        with mock.patch("time.sleep"), self.assertRaisesRegex(bridge.StepNotFound, "2 candidates"):
            owner_ui.open_room(fake, "e2e room")

    def test_private_allow_needs_agent_and_room(self):
        other_room = [n("t", "modal.body.text", "bird-codex is private ... add it to dev room?"),
                      n("a", "modal.buttons.allow", "Allow in group rooms", "press")]
        ours = [n("t2", "modal2.body.text", "bird-codex is private ... add it to e2e room?"),
                n("a2", "modal2.buttons.allow", "Allow in group rooms", "press")]
        self.assertEqual([x["id"] for x in owner_ui.private_allow(other_room + ours, "bird-codex")], ["a2"])

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

    def test_join_presses_join_room_then_yes_in_the_e2e_prompt(self):
        self.statuses = [{"owner:betty": "invite"}] * 2 + [{"owner:betty": "join"}]
        modal = "overlay.join_leave_modal.content"
        screens = [[ROOM_ROW, n("j", "dock.abc.buttons.accept_button", "Join Room", "press")],
                   [ROOM_ROW, n("j", "dock.abc.buttons.accept_button", "Join Room", "press"),
                    n("body", f"{modal}.body", 'Are you sure you want to accept this invite to join "e2e room"?'),
                    n("y", f"{modal}.buttons_view.accept_button", "Yes", "press")]]
        fake = FakeBridge(screens[0])
        orig = fake.press

        def press(node, settle=1.0):
            orig(node, settle)
            fake.screen = screens[1]
        fake.press = press
        owner_ui.join_room(fake, "!r", [])
        self.assertEqual(fake.done, [("click", "r1"), ("press", "j"), ("press", "y")])

    def test_open_prompt_for_another_room_is_not_confirmed(self):
        modal = "overlay.join_leave_modal.content"
        other = [n("body", f"{modal}.body", 'Are you sure you want to accept this invite to join "dev room"?'),
                 n("y", f"{modal}.buttons_view.accept_button", "Yes", "press")]
        self.assertEqual(owner_ui.confirm_buttons(other), [])
        self.assertEqual(owner_ui.join_buttons(other), [])

    def test_missing_join_button_is_named(self):
        self.statuses = [{"owner:betty": "invite"}]
        fake = FakeBridge([ROOM_ROW])
        with mock.patch.object(owner_ui, "wait_step", side_effect=[[ROOM_ROW], None]), \
                mock.patch.object(owner_ui, "confirm_buttons", return_value=[]):
            with self.assertRaisesRegex(bridge.StepNotFound, "Join Room"):
                owner_ui.join_room(fake, "!r", [])

    def test_add_agent_types_the_exact_mxid_then_invites(self):
        self.statuses = [{"agent:bird-codex": "absent"}] * 2 + [{"agent:bird-codex": "join"}]
        modal = "overlay.invite_modal.content"
        fake = FakeBridge([ROOM_ROW, n("plus", "search_row.new_or_join_button.icon", role="image"),
                           n("add", "menu.plus_add_agent", "Add agent to this room", "press"),
                           n("title", f"{modal}.title", "Invite to e2e room", role="static_text"),
                           n("in", f"{modal}.user_id_input", "", "set_text", role="text_field"),
                           n("ok", f"{modal}.buttons_view.confirm_button", "Invite", "press")])
        fake.type_into = lambda node, text, settle=0.5: fake.done.append(("type", node["id"], text))
        with mock.patch.object(owner_ui, "_allow_if_private"), mock.patch.object(owner_ui, "_close_invite_modal"):
            owner_ui.add_agent(fake, "!r", "bird-codex", [])
        puppet = owner_ui.config.AGENTS["bird-codex"][1]
        self.assertEqual(fake.done, [("click", "r1"), ("click", "plus"), ("press", "add"), ("type", "in", puppet),
                                     ("press", "ok")])

    def test_pending_invite_is_not_resent(self):
        self.statuses = [{"agent:bird-codex": "invite"}] * 2 + [{"agent:bird-codex": "join"}]
        fake = FakeBridge([])
        owner_ui.add_agent(fake, "!r", "bird-codex", [])
        self.assertEqual(fake.done, [])

    def test_add_agent_skips_present_agent(self):
        self.statuses = [{"agent:bird-codex": "join"}]
        fake = FakeBridge([])
        owner_ui.add_agent(fake, "!r", "bird-codex", [])
        self.assertEqual(fake.done, [])

    def test_deny_presses_only_our_card(self):
        fake = FakeBridge([ROOM_ROW, n("t", f"{NOTICE}.request_details.details", "git push e2e-1-s6"),
                           n("d", f"{NOTICE}.actions.deny", "Deny", "press"),
                           n("o", f"{NOTICE}.actions.once", "Approve once", "press")])
        orig = fake.press

        def press(node, settle=1.0):
            orig(node, settle)
            fake.screen = [ROOM_ROW]  # the card goes away once answered
        fake.press = press
        with mock.patch.object(owner_ui, "Bridge", return_value=fake):
            owner_ui.deny_card("betty", "e2e-1-s6")
        self.assertEqual(fake.done, [("click", "r1"), ("press", "d")])

    def test_deny_that_leaves_the_card_pending_is_an_error(self):
        fake = FakeBridge([ROOM_ROW, n("t", f"{NOTICE}.request_details.details", "git push e2e-1-s6"),
                           n("d", f"{NOTICE}.actions.deny", "Deny", "press")])
        with mock.patch.object(owner_ui, "Bridge", return_value=fake), mock.patch.object(owner_ui, "GONE_WAIT_S", 0), \
                self.assertRaisesRegex(bridge.StepNotFound, "still pending"):
            owner_ui.deny_card("betty", "e2e-1-s6")


if __name__ == "__main__":
    unittest.main()
