//! Demo-only default approval mode for newly created Agents.
//!
//! Compiled in only with the non-default `demo` Cargo feature, so an inherited
//! environment variable cannot relax approvals in a product build.
use hagency_agent_local::outside::MODE_OUTSIDE;

/// Opt-in default for Agents the demo creates: `outside` makes a new personal
/// Agent start in "Ask only for outside changes". Unset, or in a build without
/// the `demo` feature, it keeps the product default (ask every time).
pub const DEMO_APPROVAL_MODE_ENV: &str = "HAGENCY_DEMO_APPROVAL_MODE";

/// The approval mode a newly created Agent is saved with, when the demo sets one.
pub fn demo_default_mode() -> Option<&'static str> {
    demo_mode(std::env::var(DEMO_APPROVAL_MODE_ENV).ok().as_deref())
}

#[cfg(feature = "demo")]
fn demo_mode(value: Option<&str>) -> Option<&'static str> {
    (value?.trim() == "outside").then_some(MODE_OUTSIDE)
}
#[cfg(not(feature = "demo"))]
fn demo_mode(_value: Option<&str>) -> Option<&'static str> {
    let _ = MODE_OUTSIDE;
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "demo")]
    #[test]
    fn demo_default_mode_is_opt_in() {
        assert_eq!(demo_mode(Some("outside")), Some(MODE_OUTSIDE));
        for value in [None, Some(""), Some("ask"), Some("trusted")] {
            assert_eq!(demo_mode(value), None, "{value:?}");
        }
    }
    /// Regression: a product build must not honour `HAGENCY_DEMO_APPROVAL_MODE`.
    #[cfg(not(feature = "demo"))]
    #[test]
    fn product_build_ignores_demo_approval_mode() {
        for value in [None, Some("outside"), Some(" outside ")] {
            assert_eq!(demo_mode(value), None, "{value:?}");
        }
    }
}
