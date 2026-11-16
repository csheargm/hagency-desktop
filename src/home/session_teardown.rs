//! Logout teardown must reach the desktop dock even while it is out of sight.
//!
//! Makepad skips actions for a hidden subtree (a `PageFlip` page that is not
//! shown, or a `View` with `visible: false`; see `held_events`), and logout is
//! usually started from Settings, which hides the home page. The dock then never
//! saw `CloseAllTabs`, so the logout state machine timed out after the point of
//! no return. `HomeScreen` asks here whether the dock got this batch, and hands
//! it over directly when it carries teardown and the dock was skipped.

use std::cell::Cell;

use makepad_widgets::Actions;

use crate::{home::main_desktop_ui::MainDesktopUiAction, logout::logout_confirm_modal::LogoutAction};

thread_local! {
    static DOCK_SAW_BATCH: Cell<bool> = const { Cell::new(false) };
}

/// Called by `HomeScreen` before it forwards an action batch to its children.
pub fn begin_batch() {
    DOCK_SAW_BATCH.with(|saw| saw.set(false));
}

/// Called by the dock (`MainDesktopUI`) whenever an action batch reaches it.
pub fn dock_saw_batch() {
    DOCK_SAW_BATCH.with(|saw| saw.set(true));
}

/// Whether the batch carries teardown the dock must act on.
pub fn has_teardown(actions: &Actions) -> bool {
    actions.iter().any(|action| {
        matches!(action.downcast_ref(), Some(MainDesktopUiAction::CloseAllTabs { .. }))
            || matches!(action.downcast_ref(), Some(LogoutAction::ClearAppState { .. }))
    })
}

/// True when the dock was skipped for a batch that carries teardown.
pub fn dock_needs_delivery(actions: &Actions) -> bool {
    !DOCK_SAW_BATCH.with(Cell::get) && has_teardown(actions)
}
