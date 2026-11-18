//! The thread follow-up rule, the hint, and the content the composer sends.
use super::*;
use ruma::{owned_user_id, user_id};

fn agent(u: &UserId) -> bool {
    u.localpart().starts_with("_hagency_")
}
fn mentions(ids: &[&UserId]) -> Mentions {
    Mentions::with_user_ids(ids.iter().map(|u| (*u).to_owned()))
}
const ANNA: &str = "@csheargm:localhost:8450";
const BOB: &str = "@bob:localhost:8450";
const CLAUDE: &str = "@_hagency_csheargm_anna-claude:localhost:8450";
const CODEX: &str = "@_hagency_csheargm_anna-codex:localhost:8450";
fn id(s: &str) -> OwnedUserId {
    s.try_into().unwrap()
}

#[test]
fn requester_continues_with_the_one_agent_that_answered() {
    let (anna, claude) = (id(ANNA), id(CLAUDE));
    let root = mentions(&[&claude]);
    assert_eq!(followup_agent(&anna, &anna, Some(&root), [claude.as_ref()], agent), Some(claude.clone()));
    // Not before the Agent has answered in the thread.
    assert_eq!(followup_agent(&anna, &anna, Some(&root), [anna.as_ref()], agent), None);
    // A human named alongside the Agent does not change who continues.
    let with_bob = mentions(&[&claude, &id(BOB)]);
    assert_eq!(followup_agent(&anna, &anna, Some(&with_bob), [claude.as_ref()], agent), Some(claude));
}

#[test]
fn nobody_else_and_no_multi_agent_root_continues() {
    let (anna, bob, claude, codex) = (id(ANNA), id(BOB), id(CLAUDE), id(CODEX));
    let root = mentions(&[&claude]);
    // Another member of the thread still has to mention the Agent.
    assert_eq!(followup_agent(&bob, &anna, Some(&root), [claude.as_ref()], agent), None);
    // A root that asks several Agents has no single one to continue with.
    let both = mentions(&[&claude, &codex]);
    assert_eq!(followup_agent(&anna, &anna, Some(&both), [claude.as_ref(), codex.as_ref()], agent), None);
    // `@room` and a root that mentions no Agent at all: nothing.
    let mut room = mentions(&[&claude]);
    room.room = true;
    assert_eq!(followup_agent(&anna, &anna, Some(&room), [claude.as_ref()], agent), None);
    assert_eq!(followup_agent(&anna, &anna, Some(&mentions(&[&bob])), [bob.as_ref()], agent), None);
    assert_eq!(followup_agent(&anna, &anna, None, [claude.as_ref()], agent), None);
}

#[test]
fn a_reply_addressing_someone_else_is_sent_as_typed() {
    let (bob, claude, codex) = (id(BOB), id(CLAUDE), id(CODEX));
    let typed = |ids: &[&UserId]| RoomMessageEventContent::text_plain("look").add_mentions(mentions(ids));
    for other in [&bob, &codex] {
        let mut message = typed(&[other]);
        assert!(!address_followup(&mut message, &claude, None));
        assert!(!message.mentions.unwrap().user_ids.contains(&claude));
    }
    let mut everyone = RoomMessageEventContent::text_plain("all").add_mentions(Mentions::with_room_mention());
    assert!(!address_followup(&mut everyone, &claude, None));
    // Quoting someone else's message addresses them, not the Agent.
    let mut quoting = RoomMessageEventContent::text_plain("this?");
    assert!(!address_followup(&mut quoting, &claude, Some(&bob)));
    assert!(quoting.mentions.is_none());
    // Already mentioning the Agent, or quoting it, stays one mention.
    let mut again = typed(&[&claude]);
    assert!(address_followup(&mut again, &claude, Some(&claude)));
    assert_eq!(again.mentions.unwrap().user_ids.len(), 1);
}

/// R6, the reported flow: Anna asked "@anna-claude what are the issues on
/// repo", anna-claude answered in the thread, Anna types "are you the watcher"
/// there with no mention. What leaves the composer carries the Agent in
/// `m.mentions`, the field the agent ingress wakes on (hagency-matrix
/// `tests/intake/thread_followup.rs` feeds exactly this shape).
#[test]
fn requesters_plain_thread_reply_is_sent_mentioning_the_agent() {
    let (anna, claude) = (owned_user_id!("@csheargm:localhost:8450"), id(CLAUDE));
    let root = mentions(&[&claude]);
    let target = followup_agent(&anna, &anna, Some(&root), [claude.as_ref()], agent).unwrap();
    let mut message = RoomMessageEventContent::text_plain("are you the watcher");
    assert!(address_followup(&mut message, &target, None));
    let content = serde_json::to_value(&message).unwrap();
    assert_eq!(content["body"], "are you the watcher", "the body is sent as typed");
    assert_eq!(content["m.mentions"]["user_ids"], serde_json::json!([CLAUDE]));
    // Bob's identical plain reply in the same thread: no target, no mention.
    assert_eq!(followup_agent(user_id!("@bob:localhost:8450"), &anna, Some(&root), [claude.as_ref()], agent), None);
}

#[test]
fn the_hint_shows_the_agent_and_can_be_removed() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let hint = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        let value = script_eval!(vm, {mod.widgets.ThreadFollowupHint {}});
        let widget = WidgetRef::script_from_value(vm, value);
        widget.register_as_ui_root(vm);
        widget
    });
    let hint_ref = hint.as_thread_followup_hint();
    let target = FollowupTarget { agent: id(CLAUDE), name: "anna-claude".into() };
    assert!(!hint.visible());
    hint_ref.set_target(&mut cx, Some(target.clone()));
    assert!(hint.visible());
    assert_eq!(hint.label(&mut cx, ids!(followup_text)).text(), "Replying to anna-claude");
    assert_eq!(hint_ref.active(), Some(id(CLAUDE)));
    // What "Remove" does: the reply is then sent as typed.
    hint_ref.borrow_mut().unwrap().remove(&mut cx);
    assert!(!hint.visible());
    assert_eq!(hint_ref.active(), None);
    // The same thread keeps the removal; leaving the thread clears it.
    hint_ref.set_target(&mut cx, Some(target));
    assert_eq!(hint_ref.active(), None);
    hint_ref.set_target(&mut cx, None);
    assert!(!hint.visible());
    assert_eq!(hint_ref.active(), None);
}

#[test]
fn hint_words_have_a_chinese_translation() {
    use crate::i18n::{Language, text_for};
    for key in [REPLYING_TO_TEXT, "Remove"] {
        assert_eq!(text_for(Language::English, key), key);
        assert_ne!(text_for(Language::Chinese, key), key, "{key}");
    }
}
