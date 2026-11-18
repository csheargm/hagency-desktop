use matrix_sdk::ruma::{owned_room_id, owned_user_id};

use super::*;
use crate::profile::hover_card_route::{request_for, CardButton, CardRequest};
use crate::{profile::user_profile::UserProfile, shared::avatar::AvatarState};

fn info() -> UserProfileAndRoomId {
    UserProfileAndRoomId {
        user_profile: UserProfile {
            user_id: owned_user_id!("@ben2:hagency.test"),
            username: Some("ben2".into()),
            avatar_state: AvatarState::Unknown,
        },
        room_id: owned_room_id!("!room:hagency.test"),
    }
}

#[test]
fn view_profile_routes_a_person_to_the_side_profile() {
    let Some(CardRequest::OpenProfile(pane)) = request_for(CardButton::ViewProfile, &info(), "Lobby".into(), None) else {
        panic!("View profile must open the profile");
    };
    assert_eq!(pane.user_id, info().user_id);
    assert_eq!(pane.room_id, info().room_id);
    assert_eq!(pane.room_name, "Lobby");
    assert!(pane.room_member.is_none(), "the profile looks the member up itself");
}

#[test]
fn message_requests_the_dm_for_the_hovered_user() {
    let Some(CardRequest::Message(profile)) = request_for(CardButton::Message, &info(), String::new(), None) else {
        panic!("Message must open the DM");
    };
    assert_eq!(profile.user_id, info().user_id);
}

#[test]
fn card_sits_right_of_the_avatar_when_it_fits() {
    let anchor = Rect { pos: dvec2(100.0, 200.0), size: dvec2(32.0, 32.0) };
    assert_eq!(card_origin(anchor, dvec2(1200.0, 800.0)), (100.0 + 32.0 + CARD_GAP, 200.0));
}

#[test]
fn card_flips_left_and_stays_inside_the_window() {
    let anchor = Rect { pos: dvec2(1100.0, 790.0), size: dvec2(32.0, 32.0) };
    let (left, top) = card_origin(anchor, dvec2(1200.0, 800.0));
    assert_eq!(left, 1100.0 - CARD_GAP - CARD_WIDTH);
    assert_eq!(top, 800.0 - CARD_EST_HEIGHT - CARD_GAP);
}

fn card(cx: &mut Cx) -> AvatarHoverCard {
    cx.with_vm(|vm| {
        crate::theme::tests::install(vm, Default::default());
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.AvatarHoverCard {}});
        let card = AvatarHoverCard::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty(), "the hover card's DSL evaluates without errors");
        card
    })
}

#[test]
fn the_card_tags_agents_and_names_the_server() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut card = card(&mut cx);
    let mut agent = info();
    agent.user_profile.user_id = owned_user_id!("@_hagency_agt_1:hagency.test");
    card.target = Some(agent);
    card.populate(&mut cx);
    assert!(card.view.view(&mut cx, ids!(agent_tag)).visible());
    assert_eq!(card.view.label(&mut cx, ids!(card_details)).text(),
        "Owner not shared in this room\nSettings are visible only to its owner", "another owner's Agent shows no config");
    assert!(card.view.button(&mut cx, ids!(view_profile_button)).visible());
    assert!(!card.view.button(&mut cx, ids!(agent_details_button)).visible(), "Agent details is for own Agents only");
    card.target = Some(info());
    card.populate(&mut cx);
    assert!(!card.view.view(&mut cx, ids!(agent_tag)).visible());
    assert_eq!(card.view.label(&mut cx, ids!(card_details)).text(), "Server: hagency.test");
}

#[test]
fn agent_details_opens_only_an_owned_agent_and_copy_copies_the_id() {
    assert!(request_for(CardButton::AgentDetails, &info(), String::new(), None).is_none(), "not ours: nothing to open");
    let Some(CardRequest::OpenAgent(agent_id)) = request_for(CardButton::AgentDetails, &info(), String::new(), Some("agt_1")) else {
        panic!("Agent details must open the agent panel");
    };
    assert_eq!(agent_id, "agt_1");
    let Some(CardRequest::CopyId(id)) = request_for(CardButton::CopyId, &info(), String::new(), None) else { panic!("Copy must copy") };
    assert_eq!(id, "@ben2:hagency.test");
}

/// Flow: hovering my own Agent shows its owner, runtime and status, and
/// "Agent details" (not "View profile"), which opens its agent panel.
#[test]
fn my_agents_card_offers_agent_details_and_its_facts() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut card = card(&mut cx);
    let mut agent = info();
    agent.user_profile.user_id = owned_user_id!("@_hagency_agt_1:hagency.test");
    card.target = Some(agent.clone());
    card.facts = Some(AgentFacts {
        own_agent_id: Some("agt_1".into()),
        owner: Some(owned_user_id!("@anna:hagency.test")),
        runtime: Some("Claude"),
        status: Some("Running"),
        shared: Some(crate::hagency::room_share::DEFAULT),
    });
    card.show_facts(&mut cx);
    assert!(card.view.button(&mut cx, ids!(agent_details_button)).visible());
    assert!(!card.view.button(&mut cx, ids!(view_profile_button)).visible());
    assert_eq!(card.view.label(&mut cx, ids!(card_details)).text(),
        "Owner: @anna:hagency.test\nRuntime: Claude\nStatus: Running\nShared in this room: answers questions, reviews PRs; fixes need @anna:hagency.test's OK",
        "no settings note on my own Agent; what it does for this group room's members");
    let Some(CardRequest::OpenAgent(agent_id)) = request_for(CardButton::AgentDetails, &agent, String::new(), card.own_agent()) else {
        panic!("Agent details must open the agent panel");
    };
    assert_eq!(agent_id, "agt_1");
}
