use hagency_core::{
    project::Resource,
    qualification::{self, ModelProfile, Tier},
};
use serde_json::{Value, json};

#[test]
fn current_codex_models_preserve_role_floors_and_family() {
    for name in [
        "gpt-6.1-sol",
        "gpt-6-astra",
        "gpt-6.1-terra",
        "gpt-6-luna",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ] {
        for (effort, tier, allowed) in [
            ("low", Tier::Lightweight, vec!["documentation"]),
            (
                "medium",
                Tier::Medium,
                vec!["coding", "testing", "integration", "documentation"],
            ),
            (
                "high",
                Tier::Strong,
                vec![
                    "architect",
                    "review",
                    "coding",
                    "testing",
                    "integration",
                    "documentation",
                ],
            ),
            (
                "xhigh",
                Tier::Strong,
                vec![
                    "architect",
                    "review",
                    "coding",
                    "testing",
                    "integration",
                    "documentation",
                ],
            ),
            (
                "max",
                Tier::Strong,
                vec![
                    "architect",
                    "review",
                    "coding",
                    "testing",
                    "integration",
                    "documentation",
                ],
            ),
        ] {
            let profile = ModelProfile {
                framework: "codex".into(),
                model: name.into(),
                provider: Some("openai".into()),
                reasoning: Some(effort.into()),
            };
            assert_eq!(
                qualification::model(&profile),
                (Some(tier), Some("gpt")),
                "{name}/{effort}"
            );
            assert_eq!(
                qualification::roles()
                    .filter(|role| qualification::qualifies(&profile, role, None))
                    .collect::<Vec<_>>(),
                allowed
            );
        }
    }
}

#[test]
fn current_codex_policy_does_not_infer_models_efforts_or_providers() {
    let base = ModelProfile {
        framework: "codex".into(),
        model: "gpt-6.1-sol".into(),
        provider: Some("openai".into()),
        reasoning: Some("medium".into()),
    };
    for name in ["gpt-6.1-sol", "gpt-6-astra"] {
        for effort in [
            None,
            Some(""),
            Some("none"),
            Some("minimal"),
            Some("ultra"),
            Some("HIGH"),
        ] {
            assert_eq!(
                qualification::model(&ModelProfile {
                    model: name.into(),
                    reasoning: effort.map(str::to_owned),
                    ..base.clone()
                }),
                (None, None)
            );
        }
    }
    for profile in [
        ModelProfile {
            model: "gpt-6.1-sol-unreviewed".into(),
            ..base.clone()
        },
        ModelProfile {
            model: "gpt-future".into(),
            ..base.clone()
        },
        ModelProfile {
            framework: "other".into(),
            ..base.clone()
        },
        ModelProfile {
            provider: Some("other".into()),
            ..base
        },
    ] {
        assert_eq!(qualification::model(&profile), (None, None));
        assert!(!qualification::roles().any(|role| qualification::qualifies(&profile, role, None)));
    }
}

#[test]
fn a_model_on_the_owners_own_claude_endpoint_is_medium_whatever_its_name() {
    let own = |model: &str, framework: &str| ModelProfile {
        framework: framework.into(),
        model: model.into(),
        provider: Some(qualification::OWNER_ENDPOINT.into()),
        reasoning: Some("medium".into()),
    };
    // A name the table has never seen, and one it rates strong elsewhere.
    for name in ["glm-4.6", "claude-opus-5"] {
        let profile = own(name, "claude");
        assert_eq!(qualification::model(&profile), (Some(Tier::Medium), None));
        let roles: Vec<_> = qualification::roles()
            .filter(|role| qualification::qualifies(&profile, role, None))
            .collect();
        assert_eq!(roles, ["coding", "testing", "integration", "documentation"]);
    }
    // Only Claude Code has such a provider, and an empty name is still no model.
    assert_eq!(qualification::model(&own("glm-4.6", "codex")), (None, None));
    assert_eq!(qualification::model(&own("", "claude")), (None, None));
    // Without the marker an unknown name stays unqualified.
    let plain = ModelProfile { provider: None, ..own("glm-4.6", "claude") };
    assert_eq!(qualification::model(&plain), (None, None));
}

#[test]
fn a_claude_model_newer_than_the_table_is_rated_by_its_line() {
    let named = |model: &str, provider: Option<&str>, framework: &str| ModelProfile {
        framework: framework.into(),
        model: model.into(),
        provider: provider.map(str::to_owned),
        reasoning: Some("medium".into()),
    };
    // Names a current CLI offers, a dated one and earlier versions among
    // them; the table lists none of them.
    for (name, tier) in [
        ("claude-opus-5-5", Tier::Strong),
        ("claude-fable-5-2", Tier::Strong),
        ("claude-sonnet-5-5", Tier::Medium),
        ("claude-haiku-5-5", Tier::Lightweight),
        ("claude-haiku-4-5-20251001", Tier::Lightweight),
        ("claude-opus-4-6", Tier::Strong),
        ("claude-sonnet-4-6", Tier::Medium),
    ] {
        for provider in [None, Some("anthropic")] {
            let profile = named(name, provider, "claude");
            assert_eq!(qualification::model(&profile), (Some(tier), Some("claude")), "{name}");
        }
    }
    // A row of the table still wins over the line.
    assert_eq!(
        qualification::model(&named("claude-fable-5", None, "claude")).0,
        Some(Tier::Lightweight)
    );
    // A lightweight model rated by its line stays out of the roles the table
    // keeps its own lightweight Claude models out of, at any requested tier.
    let haiku = named("claude-haiku-5-5", None, "claude");
    for role in ["architect", "review"] {
        assert!(!qualification::qualifies(&haiku, role, Some(Tier::Lightweight)), "{role}");
    }
    assert!(qualification::roles().any(|role| qualification::qualifies(&haiku, role, None)));
    let sonnet = named("claude-sonnet-5-5", None, "claude");
    assert!(qualification::qualifies(&sonnet, "architect", Some(Tier::Medium)));
    // Not a line, not a version, another provider, another framework: unrated.
    for profile in [
        named("claude-opus", None, "claude"),
        named("claude-opus-latest", None, "claude"),
        named("claude-mystery-5", None, "claude"),
        named("opus-5-5", None, "claude"),
        named("claude-opus-5-5", Some("other"), "claude"),
        named("claude-opus-5-5", None, "codex"),
    ] {
        assert_eq!(qualification::model(&profile), (None, None), "{}", profile.model);
        assert!(!qualification::roles().any(|role| qualification::qualifies(&profile, role, None)));
    }
    // The owner's own endpoint keeps its own rule whatever the name says.
    let own = named("claude-opus-5-5", Some(qualification::OWNER_ENDPOINT), "claude");
    assert_eq!(qualification::model(&own), (Some(Tier::Medium), None));
}

#[test]
fn qualification_matches_javascript() {
    let fixtures: Value =
        serde_json::from_str(include_str!("../../fixtures/qualification.json")).unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        let profile: ModelProfile = if case["profile"].is_null() {
            ModelProfile::default()
        } else {
            serde_json::from_value(case["profile"].clone()).unwrap()
        };
        let (tier, family) = qualification::model(&profile);
        assert_eq!(json!(tier), case["tier"], "{case}");
        assert_eq!(json!(family), case["family"], "{case}");
        for (role, expected) in case["roles"].as_object().unwrap() {
            assert_eq!(
                json!(qualification::qualifies(&profile, role, None)),
                expected["default"],
                "{role}: {case}"
            );
            assert_eq!(
                json!(qualification::qualifies(
                    &profile,
                    role,
                    Some(Tier::Lightweight)
                )),
                expected["lightweight"],
                "{role}: {case}"
            );
        }
    }
    let presets: Vec<Resource> = fixtures["presets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            let mut value = value.clone();
            let id = value.as_object_mut().unwrap().remove("id").unwrap();
            value["presetId"] = id;
            value["seatId"] = json!("fixture");
            serde_json::from_value(value).unwrap()
        })
        .collect();
    for rank in fixtures["rankings"].as_array().unwrap() {
        let rows = qualification::resources_for_role(
            &presets,
            rank["role"].as_str().unwrap(),
            Some(serde_json::from_value(rank["tier"].clone()).unwrap()),
        );
        assert_eq!(
            json!(rows.iter().map(|r| &r.preset_id).collect::<Vec<_>>()),
            rank["ids"]
        );
    }
}

#[test]
fn native_resource_configuration_choices() {
    use hagency_core::qualification::{self, ModelProfile};
    for framework in ["codex", "claude", "unknown"] {
        for provider in [None, Some("openai"), Some("wrong-provider")] {
            let profile = ModelProfile {
                framework: framework.into(),
                provider: provider.map(str::to_owned),
                ..ModelProfile::default()
            };
            let choices = qualification::configuration_choices(&profile).unwrap();
            assert!(choices.len() <= 256);
            let mut unique = std::collections::BTreeSet::new();
            for c in &choices {
                assert!(unique.insert((&c.model, &c.reasoning)));
                let candidate = ModelProfile {
                    model: c.model.clone(),
                    reasoning: c.reasoning.clone(),
                    ..profile.clone()
                };
                assert_eq!(qualification::model(&candidate).0, Some(c.tier));
                assert_eq!(
                    c.roles,
                    qualification::roles()
                        .filter(|r| qualification::qualifies(&candidate, r, None))
                        .collect::<Vec<_>>()
                );
            }
            if framework == "unknown" {
                assert!(choices.is_empty());
            }
            if framework == "codex" && provider.is_none() {
                assert!(
                    choices
                        .iter()
                        .any(|c| c.model == "gpt-5.6-sol"
                            && c.reasoning.as_deref() == Some("medium"))
                );
            }
        }
    }
}
