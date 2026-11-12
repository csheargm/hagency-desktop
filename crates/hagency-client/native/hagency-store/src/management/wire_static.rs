//! Closed decoding for static display labels crossing the native IPC boundary.
//! No allocation is leaked and no host authority is reconstructed here.
pub type Label = &'static str;
use serde::{Deserialize, Deserializer, de::Error};

fn known<E: Error>(text: String) -> Result<&'static str, E> {
    const LABELS: &[&str] = &[
        super::PROTOCOL, "resource_binding", "managed_namespace", "local_configuration", "subscription",
        "api_key", "unknown", "current_task_proof_required", "health", "tasks",
        "projects", "createdAt", "binding", "repositories", "worktrees", "specs",
        "issues", "activity", "create", "edit", "delete", "accept", "transition",
        "comment", "suppressed", "pending_binding", "binding_saved",
    ];
    LABELS.iter().copied().find(|label| *label == text)
        .ok_or_else(|| E::custom("unknown native display label"))
}
pub fn one<'de, D: Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
    known(String::deserialize(d)?)
}
pub fn many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<&'static str>, D::Error> {
    Vec::<String>::deserialize(d)?.into_iter().map(known).collect()
}
pub fn optional<'de, D: Deserializer<'de>>(d: D) -> Result<Option<&'static str>, D::Error> {
    Option::<String>::deserialize(d)?.map(known).transpose()
}
