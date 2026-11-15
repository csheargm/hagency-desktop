//! Buzz-style default Agents path in the real full bundle: an Agent manager
//! lands on My Agents with no organization tabs; Advanced brings them back.
use super::operator_tasks_tests::{count, fixture_with_open};
use super::*;

fn open(advanced: bool) -> String {
    format!(r#"
let task_fixture_open=fn(){{
    H.ready=true H.busy=false H.command=nil H.entry_role="manager" H.entry_choosing=false H.advanced={advanced}
    H.session={{account:"@me:test" features:["resource-pool-agents-v1"]}}
    H.page="pool_agents"
    H.local_list=[{{id:"a1" displayName:"Coder" local:{{here:true service:{{state:"running" detail:""}}}}}}]
    A.redraw()
}}
"#)
}

fn ids_in(card: &SplashRef, id: LiveId) -> usize {
    let root = card.borrow().unwrap();
    let mut found = 0;
    root.view.children(&mut |_, child| found += count(&child, id));
    found
}

#[test]
fn simple_agents_path_shows_my_agents_without_organization_tabs() {
    let (mut cx, card) = fixture_with_open(include_str!("../../apps/hagency/bundle/main.splash"), &open(false));
    // One card, one Create agent, one Advanced link.
    assert_eq!(ids_in(&card, id!(local_agent_open)), 1);
    assert_eq!(ids_in(&card, id!(my_agents_create)), 1);
    assert_eq!(ids_in(&card, id!(my_agents_advanced)), 1);
    // No Home / Pools / Work tabs, no workspace switch, no project (pool) Agents.
    for hidden in [id!(overview), id!(projects), id!(work), id!(entry_switch), id!(my_agents_simple)] {
        assert_eq!(ids_in(&card, hidden), 0, "{hidden:?} should be hidden");
    }
    card.set_text(&mut cx, "");
}

#[test]
fn advanced_restores_organization_tabs() {
    let (mut cx, card) = fixture_with_open(include_str!("../../apps/hagency/bundle/main.splash"), &open(true));
    for shown in [id!(overview), id!(projects), id!(work), id!(my_agents_simple)] {
        assert_eq!(ids_in(&card, shown), 1, "{shown:?} should show in Advanced");
    }
    assert_eq!(ids_in(&card, id!(my_agents_advanced)), 0);
    card.set_text(&mut cx, "");
}
