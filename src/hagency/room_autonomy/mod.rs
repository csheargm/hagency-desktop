//! The room's work loop controls, desktop side (ADR 0030): the autonomy level
//! (Off · Assist · Autonomous) and Pause / Resume, read from the homeserver
//! itself ([`state`], [`server`]) and recorded per room through the owner API
//! so the gate and push trust judge by fresh values. Who may do what follows
//! the room's power levels: the room's owner (admin) and maintainers (power
//! level 50) change the level and resume; every member may pause.
pub mod gate;
pub mod lines;
pub mod server;
pub mod state;
pub mod stop;
pub mod writes;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod lines_tests;
