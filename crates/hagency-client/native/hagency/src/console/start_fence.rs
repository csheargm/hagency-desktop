//! The owner's kill switch, enforced where every Agent start passes: the
//! native owner engine refuses `StartAgentService` and `RuntimeStart` for an
//! Agent its owner paused. That covers Desktop's buttons, Create agent,
//! assignment and boot intent recovery, in Desktop's process and in the
//! owner daemon alike. The host installs the check once at process start;
//! without one, nothing is refused.
use std::sync::OnceLock;

/// The refusal code of a start while the owner paused the Agent.
pub const PAUSED_CODE: &str = "agents_paused_by_owner";

static FENCE: OnceLock<fn(&str) -> bool> = OnceLock::new();

/// Install `paused(agent_id)`. Only the first call takes effect.
pub fn install(paused: fn(&str) -> bool) -> bool {
    FENCE.set(paused).is_ok()
}

/// Whether a start of `agent` must be refused now.
pub fn refuses(agent: &str) -> bool {
    FENCE.get().is_some_and(|paused| paused(agent))
}
