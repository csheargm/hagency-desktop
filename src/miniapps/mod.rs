//! Octoscript mini-app hosting with account-bound Matrix and Octos service access.

pub use rinx_miniapp_core::{InstanceId, Lease, OctosProvider, ServiceEvent};
use std::sync::LazyLock;
static AUTHORITY: LazyLock<rinx_miniapp_core::SessionAuthority> = LazyLock::new(Default::default);

pub fn invalidate_sessions() {
    AUTHORITY.invalidate();
    // Account change or logout: the assistant's contexts of the previous
    // account are revoked too (ADR 0007).
    crate::octos_service::revoke_account();
    // And the assistant's waiting and running calls.
    crate::assistant::invalidate();
}

/// A lease for one assistant request: the app `assistant`, this account,
/// this one room and this one service. The Matrix adapters check it like a
/// mini app's, so an account switch or logout revokes it mid-flight.
pub(crate) fn assistant_lease(account: &str, room: &str, service: &str) -> Lease {
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    AUTHORITY.issue(
        InstanceId {
            app: "assistant".into(),
            account: account.into(),
            room: None,
            generation: GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        },
        [service.to_owned()].into(),
        [room.to_owned()].into(),
        std::time::Instant::now() + std::time::Duration::from_secs(120),
    )
}

pub async fn matrix_request(
    lease: Lease,
    service: String,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let client = crate::sliding_sync::get_client().ok_or("Not logged in")?;
    crate::host::matrix::execute(client, lease, service, args).await
}


pub mod action_room;
mod catalog_worker;
mod consent;
mod design;
mod library;
mod package;
pub mod palpo;
mod private_export;
pub mod presentation;
mod sandbox;
pub mod ui;
pub mod side;
pub mod surface;
pub mod window;
pub use crate::host::octos::ContextProvider;
pub use ui::{MiniAppsAction, MiniAppsPanelWidgetRefExt};
pub use crate::host::hagency::tool_approvals::ToolApprovalTarget;

pub fn script_mod(vm: &mut makepad_widgets::ScriptVm) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // JIT PORT: the fork's script VM ends an evaluation at an uncaught error; upstream's
        // reports it and runs on unless the host asks. Mini-app script is not ours: ask.
        makepad_widgets::widget_async::set_splash_bail_on_uncaught_error(true);
        makepad_widgets::widget_async::register_splash_isolate_mod(crate::theme::script_mod);
        makepad_widgets::widget_async::register_splash_isolate_mod(crate::i18n::script_mod);
        makepad_widgets::widget_async::register_splash_isolate_mod(|vm| {
            design::install_system_cjk_fallback(vm);
            design::script_mod(vm);
        });
        makepad_widgets::widget_async::register_splash_isolate_mod(|vm| {
            octoscript_widgets::design::script_mod(vm);
        });
        makepad_widgets::widget_async::register_splash_isolate_mod(|vm| {
            octoscript_widgets::kit::script_mod(vm);
        });
        makepad_widgets::widget_async::register_splash_isolate_mod(|vm| {
            octoscript_widgets::tap::script_mod(vm);
        });
        makepad_widgets::widget_async::register_splash_isolate_mod(
            makepad_widgets::splash::register_agent_module,
        );
    });
    // Trusted native confirmations also use the shared number editor. Register
    // its types in the host VM as well as in each contained Splash isolate.
    design::script_mod(vm);
    library::script_mod(vm);
    ui::script_mod(vm);
    action_room::script_mod(vm);
}
