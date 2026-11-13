use hagency_runtime::octos::profile_binding::{Error, ProfileBinding, profile_ids};
use serde_json::json;
use std::{
    fs,
    time::{Duration, Instant},
};

fn expires() -> Instant {
    Instant::now() + Duration::from_secs(30)
}
fn profile(id: &str, family: &str, model: &str, credential: &str) -> String {
    json!({"id":id,"config":{"llm":{"primary":{"family_id":family,"model_id":model}},
        "env_vars":{"FIXTURE_TOKEN":credential}}})
    .to_string()
}

#[test]
#[cfg(unix)]
fn octos_profiles_have_independent_budgets_but_edits_keep_the_same_profile_key() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    let path = home.join("profiles/coding.json");
    fs::write(&path, profile("coding", "local", "test", "fixture-secret")).unwrap();
    fs::write(
        home.join("profiles/second.json"),
        profile("second", "local", "test", "fixture-secret"),
    )
    .unwrap();
    let before = ProfileBinding::observe(&home, "coding", expires()).unwrap();
    let other = ProfileBinding::observe(&home, "second", expires()).unwrap();
    let key = before.accounting_key(&[7; 32]).unwrap();
    let version = before.configuration_key(&[7; 32]).unwrap();
    assert_ne!(key, other.accounting_key(&[7; 32]).unwrap());
    assert_ne!(key, before.accounting_key(&[8; 32]).unwrap());
    assert_eq!(before.accounting_key(&[0; 32]).unwrap_err(), Error::Invalid);

    // Even a change to a non-model field invalidates an in-flight witness.
    // Re-observation uses the same budget slot, not a new provider account.
    fs::write(&path, profile("coding", "local", "test", "rotated-fixture")).unwrap();
    assert_eq!(before.check().unwrap_err(), Error::Unavailable);
    let after = ProfileBinding::observe(&home, "coding", expires()).unwrap();
    assert_eq!(key, after.accounting_key(&[7; 32]).unwrap());
    assert_ne!(version, after.configuration_key(&[7; 32]).unwrap());
    assert_ne!(
        after.configuration_key(&[7; 32]).unwrap(),
        after.configuration_key(&[8; 32]).unwrap()
    );
    assert_eq!(after.model().model, "test");
    assert!(!key.contains("fixture"));
}

#[test]
#[cfg(unix)]
fn one_profile_exposes_configured_models_with_one_budget_and_exact_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("owner");
    fs::create_dir_all(home.join("profiles")).unwrap();
    let original = json!({"id":"coding","config":{"llm":{
        "primary":{"family_id":"local","model_id":"qwen-local","route":{"base_url":"http://127.0.0.1:11/v1"}},
        "fallbacks":[
            {"family_id":"local","model_id":"deepseek-local","route":{"base_url":"http://127.0.0.1:22/v1"}},
            {"family_id":"local","model_id":"ambiguous","route":{"route_id":"one"}},
            {"family_id":"local","model_id":"ambiguous","route":{"route_id":"two"}},
            {"family_id":"local","model_id":"qwen-local","route":{"route_id":"shadow"}}
        ]},"env_vars":{"TOKEN":"synthetic-secret"}}});
    let path = home.join("profiles/coding.json");
    fs::write(&path, original.to_string()).unwrap();
    let primary = ProfileBinding::observe(&home, "coding", expires()).unwrap();
    let alternative = ProfileBinding::observe_model(&home, "coding", "deepseek-local", expires()).unwrap();
    assert_eq!(primary.models().unwrap().iter().map(|model| model.model.as_str()).collect::<Vec<_>>(), ["qwen-local", "deepseek-local"]);
    assert!(ProfileBinding::observe_model(&home, "coding", "ambiguous", expires()).is_err());
    assert!(ProfileBinding::observe_model(&home, "coding", "not-configured", expires()).is_err());
    assert_eq!(primary.accounting_key(&[7; 32]).unwrap(), alternative.accounting_key(&[7; 32]).unwrap());
    assert_eq!(primary.configuration_key(&[7; 32]).unwrap(), alternative.configuration_key(&[7; 32]).unwrap());
    assert_ne!(primary.selected_model_key(&[7; 32]).unwrap(), alternative.selected_model_key(&[7; 32]).unwrap());
    for (name, binding, expected) in [("primary", &primary, "qwen-local"), ("alternative", &alternative, "deepseek-local")] {
        let destination = root.join(name); fs::create_dir(&destination).unwrap();
        binding.snapshot_into(&destination).unwrap();
        let scoped: serde_json::Value = serde_json::from_slice(&fs::read(destination.join("profiles/coding.json")).unwrap()).unwrap();
        assert_eq!(scoped["config"]["llm"]["primary"]["model_id"], expected);
        assert_eq!(scoped["config"]["llm"]["fallbacks"], json!([]));
    }
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap(), original);
    let original_model_key = primary.selected_model_key(&[7; 32]).unwrap();
    let mut with_another_model = original.clone();
    with_another_model["updated_at"] = json!("2026-11-12T12:00:00Z");
    with_another_model["config"]["llm"]["fallbacks"].as_array_mut().unwrap()
        .push(json!({"family_id":"local","model_id":"another-local"}));
    fs::write(&path, with_another_model.to_string()).unwrap();
    let refreshed = ProfileBinding::observe(&home, "coding", expires()).unwrap();
    assert_eq!(refreshed.selected_model_key(&[7; 32]).unwrap(), original_model_key,
        "adding an unrelated fallback must not change an existing model's runtime binding");
    fs::write(&path, original.to_string()).unwrap();
    let guard = alternative.running_guard().unwrap();
    fs::write(&path, profile("coding", "local", "qwen-local", "changed")).unwrap();
    assert!(guard.check().is_err());
}

#[test]
#[cfg(unix)]
fn configured_model_list_never_exceeds_the_inventory_bound() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    let path = home.join("profiles/coding.json");
    let mut profile = json!({"id":"coding","config":{"llm":{
        "primary":{"family_id":"local","model_id":"primary"},
        "fallbacks":(0..63).map(|i|json!({"family_id":"local","model_id":format!("model-{i}")})).collect::<Vec<_>>()}}});
    fs::write(&path, profile.to_string()).unwrap();
    assert_eq!(ProfileBinding::observe(&home, "coding", expires()).unwrap().models().unwrap().len(), 64);
    profile["config"]["llm"]["fallbacks"].as_array_mut().unwrap().push(json!({"family_id":"local","model_id":"model-overflow"}));
    fs::write(&path, profile.to_string()).unwrap();
    assert!(ProfileBinding::observe(&home, "coding", expires()).is_err());
}

#[test]
#[cfg(unix)]
fn running_guard_outlives_admission_witness_but_detects_profile_and_defaults_edits() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    let path = home.join("profiles/coding.json");
    fs::write(&path, profile("coding", "local", "test", "fixture")).unwrap();
    let binding = ProfileBinding::observe(&home, "coding", Instant::now() + Duration::from_millis(100)).unwrap();
    let guard = binding.running_guard().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert!(binding.check().is_err());
    assert!(binding.running_guard().is_err(), "an expired witness cannot create a new guard");
    assert!(guard.check().is_ok(), "an existing process can outlive the witness");
    fs::write(home.join("profile-defaults.json"), b"{}").unwrap();
    assert!(guard.check().is_err());
    fs::remove_file(home.join("profile-defaults.json")).unwrap();
    assert!(guard.check().is_ok());
    fs::write(path, profile("coding", "local", "test", "rotated")).unwrap();
    assert!(guard.check().is_err());
}

#[test]
#[cfg(unix)]
fn octos_profile_bindings_reject_bad_ids_missing_profiles_and_expired_witnesses() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    assert!(matches!(
        ProfileBinding::observe(&home, "../escape", expires()),
        Err(Error::Invalid)
    ));
    assert!(matches!(
        ProfileBinding::observe(&home, "missing", expires()),
        Err(Error::Unavailable)
    ));
    assert!(matches!(
        ProfileBinding::observe(&home, "coding", Instant::now()),
        Err(Error::Invalid)
    ));
    fs::write(home.join("profiles/coding.json"), b"{}").unwrap();
    assert!(matches!(
        ProfileBinding::observe(&home, "coding", expires()),
        Err(Error::Unavailable)
    ));
}

#[cfg(unix)]
#[test]
fn inherited_defaults_change_the_version_without_creating_a_new_budget() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    fs::write(
        home.join("profiles/coding.json"),
        profile("coding", "local", "test", "fixture"),
    )
    .unwrap();
    let before = ProfileBinding::observe(&home, "coding", expires()).unwrap();
    let account = before.accounting_key(&[7; 32]).unwrap();
    let version = before.configuration_key(&[7; 32]).unwrap();
    fs::write(
        home.join("profile-defaults.json"),
        b"{\"env_vars\":{\"TOKEN\":\"fixture\"}}",
    )
    .unwrap();
    assert_eq!(before.check().unwrap_err(), Error::Unavailable);
    let after = ProfileBinding::observe(&home, "coding", expires()).unwrap();
    assert_eq!(account, after.accounting_key(&[7; 32]).unwrap());
    assert_ne!(version, after.configuration_key(&[7; 32]).unwrap());
    fs::remove_file(home.join("profile-defaults.json")).unwrap();
    assert_eq!(after.check().unwrap_err(), Error::Unavailable);
}

#[cfg(not(unix))]
#[test]
fn octos_profile_binding_is_unavailable_without_a_safe_open() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    fs::write(
        home.join("profiles/coding.json"),
        profile("coding", "local", "test", "fixture"),
    )
    .unwrap();
    assert!(matches!(
        ProfileBinding::observe(&home, "coding", expires()),
        Err(Error::Unavailable)
    ));
}

#[cfg(unix)]
#[test]
fn octos_profile_binding_does_not_follow_a_profile_symlink() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    fs::write(
        home.join("outside"),
        profile("coding", "local", "test", "fixture"),
    )
    .unwrap();
    symlink(home.join("outside"), home.join("profiles/coding.json")).unwrap();
    assert!(matches!(
        ProfileBinding::observe(&home, "coding", expires()),
        Err(Error::Unavailable)
    ));
    fs::remove_file(home.join("profiles/coding.json")).unwrap();
    fs::write(
        home.join("profiles/coding.json"),
        profile("coding", "local", "test", "fixture"),
    )
    .unwrap();
    symlink(home.join("outside"), home.join("profile-defaults.json")).unwrap();
    assert!(matches!(
        ProfileBinding::observe(&home, "coding", expires()),
        Err(Error::Unavailable)
    ));
}

#[cfg(unix)]
#[test]
fn octos_inventory_is_bounded_sorted_and_never_follows_the_profiles_directory() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    for id in ["zeta", "alpha"] {
        fs::write(
            home.join("profiles").join(format!("{id}.json")),
            profile(id, "local", "test", "fixture"),
        )
        .unwrap();
    }
    fs::write(home.join("profiles/bad name.json"), b"{}").unwrap();
    assert_eq!(profile_ids(&home).unwrap(), vec!["alpha", "zeta"]);
    for index in 0..63 {
        fs::write(
            home.join("profiles").join(format!("p{index:02}.json")),
            b"{}",
        )
        .unwrap();
    }
    assert_eq!(profile_ids(&home).unwrap_err(), Error::Unavailable);
    fs::rename(home.join("profiles"), home.join("actual-profiles")).unwrap();
    symlink(home.join("actual-profiles"), home.join("profiles")).unwrap();
    assert_eq!(profile_ids(&home).unwrap_err(), Error::Unavailable);
}

#[cfg(unix)]
#[test]
fn octos_scoped_snapshot_contains_only_the_selected_profile_and_rejects_stale_or_occupied_destinations()
 {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("owner");
    let selected = root.join("selected");
    let occupied = root.join("occupied");
    for path in [&home, &selected, &occupied] {
        fs::create_dir(path).unwrap();
    }
    fs::create_dir(home.join("profiles")).unwrap();
    fs::write(
        home.join("profiles/coding.json"),
        profile("coding", "local", "test", "selected-secret"),
    )
    .unwrap();
    fs::write(
        home.join("profiles/other.json"),
        profile("other", "local", "test", "other-secret"),
    )
    .unwrap();
    fs::write(
        home.join("profile-defaults.json"),
        b"{\"env_vars\":{\"TOKEN\":\"defaults-secret\"}}",
    )
    .unwrap();
    let binding = ProfileBinding::observe(&home, "coding", expires()).unwrap();
    binding.snapshot_into(&selected).unwrap();
    assert_eq!(
        fs::read_to_string(selected.join("profiles/coding.json")).unwrap(),
        fs::read_to_string(home.join("profiles/coding.json")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(selected.join("profile-defaults.json")).unwrap(),
        fs::read_to_string(home.join("profile-defaults.json")).unwrap()
    );
    assert!(!selected.join("profiles/other.json").exists());
    assert_eq!(
        fs::metadata(selected.join("profiles/coding.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(selected.join("profile-defaults.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::write(occupied.join("profile-defaults.json"), b"unrelated").unwrap();
    assert!(binding.snapshot_into(&occupied).is_err());
    assert_eq!(
        fs::read(occupied.join("profile-defaults.json")).unwrap(),
        b"unrelated"
    );
    assert!(!occupied.join("profiles").exists());
    let alias = root.join("alias");
    symlink(&occupied, &alias).unwrap();
    assert!(binding.snapshot_into(&alias).is_err());
    let stale = root.join("stale");
    fs::create_dir(&stale).unwrap();
    fs::write(
        home.join("profiles/coding.json"),
        profile("coding", "local", "test", "rotated"),
    )
    .unwrap();
    assert!(binding.snapshot_into(&stale).is_err());
    assert_eq!(fs::read_dir(stale).unwrap().count(), 0);
}
