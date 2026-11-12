//! Modules for saving/restoring  application data to persistent storage.

/// For persisting the state of a user's logged-in Matrix session.
pub mod matrix_state;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub(crate) mod credential_state;
#[cfg(any(target_os = "ios", target_os = "android", test))]
pub(crate) mod credential_state_mobile;
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) use credential_state_mobile as credential_state;
pub use matrix_state::*;

/// For persisting application state not related to Matrix.
pub mod app_state;
pub use app_state::*;

pub mod space_graph;

/// For persisting state related to TSP wallets and identities.
#[cfg(feature = "tsp")]
pub mod tsp_state;
#[cfg(feature = "tsp")]
pub use tsp_state::*;
