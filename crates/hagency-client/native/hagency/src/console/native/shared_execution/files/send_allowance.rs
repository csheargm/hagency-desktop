//! Bound automatic shared-room publication without interrupting the model turn.
//! Admission precedes network I/O and survives host restart. Exact retries use
//! their original allowance; an uncertain upload still consumes its allowance.
use super::*;
use std::collections::BTreeMap;

pub(super) struct SendAllowance {
    pub(super) count: usize,
    pub(super) bytes: usize,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Admissions {
    files: BTreeMap<String, Admission>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Admission {
    selection: String,
    bytes: usize,
}

impl SendAllowance {
    pub(super) fn shared() -> Self {
        Self { count: 64, bytes: 64 * 1024 * 1024 }
    }

    pub(super) fn admit(
        &self,
        store: &Store,
        scope: &str,
        id: &str,
        selection: &str,
        bytes: usize,
    ) -> Result<()> {
        let ledger = format!("send_allowance_{scope}");
        let saved = store.read::<Admissions>(&ledger)?;
        let exists = saved.is_some();
        let mut saved = saved.unwrap_or_default();
        if let Some(original) = saved.files.get(id) {
            return if original.selection == selection && original.bytes == bytes {
                Ok(())
            } else {
                Err("file_call_id_conflict")
            };
        }
        let total = saved.files.values().try_fold(bytes, |sum, item| sum.checked_add(item.bytes));
        if saved.files.len() >= self.count || total.is_none_or(|total| total > self.bytes) {
            return Err("file_send_turn_limit");
        }
        saved.files.insert(id.into(), Admission { selection: selection.into(), bytes });
        if exists {
            store.update(&ledger, &saved)
        } else if store.insert(&ledger, &saved)? {
            Ok(())
        } else {
            // Another host already admitted work for this scope. Never replace
            // its accounting or race a second upload.
            Err("file_send_outcome_unknown")
        }
    }
}
