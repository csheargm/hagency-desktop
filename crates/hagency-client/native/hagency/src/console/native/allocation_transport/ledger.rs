//! Every owner database numbers its own mutations from 1, and receipts carry
//! those numbers. Before this device reports anything it names the database
//! to the server, so a replaced database opens a new number space there
//! instead of colliding with numbers the old database already reported.
use super::*;

/// How long to wait before asking a server that predates declarations again.
const ASK_AGAIN: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(in crate::console::native) enum LedgerDeclaration {
    #[default]
    Unknown,
    /// The server numbers this device's receipts in this database's space.
    Declared {
        device: DeviceIdentity,
        ledger_id: String,
    },
    /// The server does not know declarations. It keeps one number space per
    /// device, as before.
    Unsupported {
        device: DeviceIdentity,
        ledger_id: String,
        until: Instant,
    },
}
impl LedgerDeclaration {
    fn settles(&self, device: &DeviceIdentity, ledger_id: &str, now: Instant) -> bool {
        match self {
            Self::Unknown => false,
            Self::Declared {
                device: d,
                ledger_id: l,
            } => d == device && l == ledger_id,
            Self::Unsupported {
                device: d,
                ledger_id: l,
                until,
            } => d == device && l == ledger_id && now < *until,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Declared {
    ledger_id: String,
    epoch: u64,
}

/// What a declaration reply means for the requests that follow it.
fn outcome(
    ledger_id: &str,
    reply: Result<Declared, NativeError>,
) -> Result<bool, NativeError> {
    match reply {
        Ok(declared) if declared.ledger_id == ledger_id && declared.epoch >= 1 => Ok(true),
        Ok(_) => Err(invalid()),
        // An older server answers an unknown management route this way.
        Err(error) if error.status == 404 && error.code == "not_found" => Ok(false),
        // Includes `agent_executor_ledger_retired`: this device reported from
        // this database before and has since reported from another one, so
        // its numbers are spent. Nothing may be uploaded from it.
        Err(error) => Err(error),
    }
}

impl AllocationTransport {
    /// Runs before every request that can lease work or carry a receipt.
    /// One declaration per device and database for the life of this owner;
    /// a failed one is repeated by the next request and blocks it meanwhile.
    pub(super) async fn declare_ledger(&self, scope: &Scope) -> Result<(), NativeError> {
        let Some(ledger_id) = self.manager.ledger_id() else {
            return Ok(());
        };
        let device = DeviceIdentity::from(&scope.device);
        let mut state = self.manager.owner.0.management.ledger.lock().await;
        if state.settles(&device, &ledger_id, Instant::now()) {
            return Ok(());
        }
        let reply = self
            .send::<Declared>(scope, Operation::DeclareLedger(ledger_id.clone()), false)
            .await;
        let epoch = reply.as_ref().map_or(0, |declared| declared.epoch);
        match outcome(&ledger_id, reply) {
            Ok(true) => {
                tracing::info!(epoch, "Owner ledger declared to the server");
                *state = LedgerDeclaration::Declared { device, ledger_id };
                Ok(())
            }
            Ok(false) => {
                *state = LedgerDeclaration::Unsupported {
                    device,
                    ledger_id,
                    until: Instant::now() + ASK_AGAIN,
                };
                Ok(())
            }
            Err(error) => {
                tracing::warn!(code = %error.code, "Owner ledger declaration failed; uploads wait for it");
                *state = LedgerDeclaration::Unknown;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str) -> DeviceIdentity {
        DeviceIdentity {
            origin: "https://server.test/".into(),
            issuer: "https://server.test/auth/".into(),
            subject: "subject".into(),
            owner: "@owner:test".into(),
            user: "uid".into(),
            id: id.into(),
            generation: 1,
        }
    }

    #[test]
    fn a_declaration_settles_only_its_own_device_and_database() {
        let now = Instant::now();
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        assert!(!LedgerDeclaration::Unknown.settles(&device("d"), &a, now));
        let declared = LedgerDeclaration::Declared {
            device: device("d"),
            ledger_id: a.clone(),
        };
        assert!(declared.settles(&device("d"), &a, now));
        // A replaced database or another device declares again.
        assert!(!declared.settles(&device("d"), &b, now));
        assert!(!declared.settles(&device("e"), &a, now));
        let old_server = LedgerDeclaration::Unsupported {
            device: device("d"),
            ledger_id: a.clone(),
            until: now + ASK_AGAIN,
        };
        assert!(old_server.settles(&device("d"), &a, now));
        // An upgraded server is asked again after the wait.
        assert!(!old_server.settles(&device("d"), &a, now + ASK_AGAIN));
        assert!(!old_server.settles(&device("d"), &b, now));
    }

    #[test]
    fn declaration_replies_open_the_space_tolerate_an_old_server_or_block_uploads() {
        let id = "a".repeat(64);
        let declared = |ledger_id: &str, epoch| {
            Ok(Declared {
                ledger_id: ledger_id.into(),
                epoch,
            })
        };
        assert_eq!(outcome(&id, declared(&id, 1)).unwrap(), true);
        assert_eq!(outcome(&id, declared(&id, 7)).unwrap(), true);
        // A reply for another database, or without an epoch, is not an answer.
        assert_eq!(
            outcome(&id, declared(&"b".repeat(64), 1)).unwrap_err().code,
            "invalid_allocation_response"
        );
        assert_eq!(
            outcome(&id, declared(&id, 0)).unwrap_err().code,
            "invalid_allocation_response"
        );
        assert_eq!(outcome(&id, Err(failure(404, "not_found"))).unwrap(), false);
        for (status, code) in [
            (409, "agent_executor_ledger_retired"),
            (404, "management_not_found"),
            (503, "server_unavailable"),
            (401, "management_scope_changed"),
        ] {
            let error = outcome(&id, Err(failure(status, code))).unwrap_err();
            assert_eq!((error.status, error.code.as_str()), (status, code));
        }
    }
}
