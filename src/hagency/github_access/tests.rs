use super::test_support::{checked, finish, stored};
use super::text::{access_text, error_text, report_lines};
use ::hagency::github_access::check::{CheckError, CheckReport, RepoAccess};
use makepad_widgets::*;

fn section(cx: &mut Cx) -> super::GithubAccessSection {
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.GithubAccessSection {}});
        let section = super::GithubAccessSection::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty());
        section
    })
}

#[test]
fn check_result_names_the_login_and_each_repos_access() {
    let report = CheckReport {
        login: "csheargm".into(),
        repos: vec![
            ("csheargm/hagency-desktop".into(), RepoAccess::Granted { push: true, pull: true }),
            ("hagency-org/hagency-desktop".into(), RepoAccess::Granted { push: false, pull: true }),
            ("csheargm/gone".into(), RepoAccess::NotReachable),
        ],
    };
    assert_eq!(
        report_lines(&report),
        "Signed in to GitHub as csheargm.\ncsheargm/hagency-desktop: push and pull ✓\nhagency-org/hagency-desktop: pull only (cannot push)\ncsheargm/gone: not reachable with this token"
    );
    let empty = CheckReport { login: "a".into(), repos: vec![] };
    assert!(report_lines(&empty).contains("set one under Issue watching"));
    assert_eq!(access_text(&RepoAccess::Status(403)), "GitHub answered 403");
}

#[test]
fn check_failures_say_what_went_wrong() {
    assert!(error_text(&CheckError::Unauthorized).contains("refused the token (401)"));
    assert_eq!(error_text(&CheckError::Status(500)), "Check failed: GitHub answered 500.");
    assert!(error_text(&CheckError::Http("timed out".into())).ends_with("timed out"));
}

#[test]
fn saved_token_is_never_shown_and_unlocks_check_and_remove() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut panel = section(&mut cx);
    panel.agent_id = "agt_1".into();
    finish(&mut panel, &mut cx, stored(false, ""));
    assert_eq!(panel.label(&mut cx, ids!(token_state)).text(), "No token saved for this Agent.");
    assert!(!panel.button(&mut cx, ids!(check_token)).borrow().unwrap().enabled());
    panel.view.text_input(&mut cx, ids!(token_input)).set_text(&mut cx, "two words");
    panel.save(&mut cx);
    assert_eq!(panel.label(&mut cx, ids!(token_note)).text(), "That does not look like a GitHub token.");
    finish(&mut panel, &mut cx, stored(true, "Token saved."));
    assert_eq!(panel.label(&mut cx, ids!(token_note)).text(), "Token saved.");
    assert_eq!(panel.label(&mut cx, ids!(token_state)).text(), "A token is saved for this Agent (hidden).");
    assert!(panel.button(&mut cx, ids!(check_token)).borrow().unwrap().enabled());
    finish(&mut panel, &mut cx, checked(Err(CheckError::Unauthorized)));
    assert!(panel.label(&mut cx, ids!(token_note)).text().contains("401"));
}
