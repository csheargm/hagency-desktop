use super::*;

#[test]
fn late_profile_fills_empty_settings_but_preserves_edits_and_account_identity() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut settings = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        let value = script_eval!(vm, {mod.widgets.AccountSettings {}});
        AccountSettings::script_from_value(vm, value)
    });
    let profile = |user_id, username| UserProfile {user_id, username, avatar_state: AvatarState::Known(None)};
    let alice = ruma::user_id!("@alice:test").to_owned();
    let input = settings.view.text_input(&cx, ids!(display_name_input));
    for draft in ["", "My unsaved name"] {
        settings.own_profile = Some(profile(alice.clone(), None));
        settings.awaiting_profile = true;
        input.set_text(&mut cx, draft);
        settings.complete_pending_profile(&mut cx, profile(ruma::user_id!("@other:test").to_owned(), Some("Other".into())));
        assert!(settings.awaiting_profile);
        assert_eq!(input.text(), draft);
        settings.complete_pending_profile(&mut cx, profile(alice.clone(), Some("Alice".into())));
        assert!(!settings.awaiting_profile);
        assert_eq!(input.text(), if draft.is_empty() {"Alice"} else {draft});
        assert_eq!(settings.own_profile.as_ref().unwrap().username.as_deref(), Some("Alice"));
    }
    settings.awaiting_profile = true;
    settings.handle_actions(&mut cx, &[Box::new(AccountDataAction::DisplayNameChanged(Some("Saved name".into())))]);
    settings.complete_pending_profile(&mut cx, profile(alice.clone(), Some("Older name".into())));
    assert_eq!(input.text(), "Saved name");
    assert_eq!(settings.own_profile.as_ref().unwrap().username.as_deref(), Some("Saved name"));

    settings.awaiting_profile = true;
    settings.handle_actions(&mut cx, &[Box::new(AccountDataAction::AvatarChanged(None))]);
    let mut late = profile(alice, Some("Fetched name".into()));
    late.avatar_state = AvatarState::Known(Some("mxc://test/old-avatar".into()));
    settings.complete_pending_profile(&mut cx, late);
    assert_eq!(input.text(), "Fetched name", "An avatar edit must not prevent the display name from loading");
    assert!(matches!(settings.own_profile.as_ref().unwrap().avatar_state, AvatarState::Known(None)),
        "A late profile must not restore the removed avatar");
}
