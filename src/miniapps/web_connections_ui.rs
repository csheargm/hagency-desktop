//! Native connection review and document custody. Only public results enter Splash.
use super::*;
use crate::miniapps::private_export;
use crate::host::hagency::web_connections::{ConnectionConsent, ConnectionOutcome};

pub(super) struct ConnectionPrompt {
    account: String,
    expires: Instant,
    receiver: Option<Receiver<Result<ConnectionConsent, String>>>,
    consent: Option<ConnectionConsent>,
    failure: Option<String>,
}
impl ConnectionPrompt {
    pub(super) fn begin(consent: ConnectionConsent) -> Self {
        let account = consent.account().to_string();
        let (tx, rx) = mpsc::sync_channel(1);
        crate::sliding_sync::spawn_async_task(async move {
            let private = if consent.needs_file() {
                let (lease, account) = consent.picker_identity();
                match pick_configuration(lease, account).await {
                    Ok(Some(text)) => Ok(Some(text)),
                    Ok(None) => Err("web_connection_cancelled".into()),
                    Err(e) => Err(e),
                }
            } else {
                Ok(None)
            };
            let result = match private {
                Ok(text) => consent.load(text).await,
                Err(e) => Err(e),
            };
            let _ = tx.try_send(result);
            SignalToUI::set_ui_signal();
        });
        Self {
            account,
            expires: Instant::now() + Duration::from_secs(180),
            receiver: Some(rx),
            consent: None,
            failure: None,
        }
    }
    pub(super) fn account(&self) -> &str {
        &self.account
    }
    pub(super) fn expired(&self) -> bool {
        Instant::now() >= self.expires
            || self
                .consent
                .as_ref()
                .is_some_and(ConnectionConsent::expired)
    }
    pub(super) fn ready(&self) -> bool {
        self.receiver.is_none()
            && self.consent.as_ref().is_some_and(ConnectionConsent::ready)
            && !self.expired()
    }
    pub(super) fn poll(&mut self) -> bool {
        let Some(rx) = &self.receiver else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(v) => v,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(_) => Err("management_unavailable".into()),
        };
        self.receiver = None;
        match result {
            Ok(v) => {
                self.consent = Some(v);
                self.failure = None
            }
            Err(e) => self.failure = Some(e),
        }
        true
    }
    pub(super) fn words(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        self.consent
            .as_ref()
            .map(ConnectionConsent::words)
            .unwrap_or((
                "Review project connection",
                "Choose a private JSON configuration when the system picker opens.",
                "Credentials stay native and are not sent to the mini app.",
                "Save configuration",
            ))
    }
    pub(super) fn details(&self) -> String {
        let mut text = self
            .consent
            .as_ref()
            .map(ConnectionConsent::details)
            .unwrap_or_else(|| crate::i18n::tr("Reading connection configuration…").into());
        if let Some(code) = &self.failure {
            text.push_str("\n\n");
            text.push_str(crate::i18n::tr(match code.as_str(){
                "web_connection_cancelled"=>"File selection was cancelled. No connection was changed.",
                "connection_import_server_mismatch"=>"This file belongs to a different Matrix server. Cancel and check the selected server.",
                "invalid_connection_configuration"|"invalid_management_arguments"=>"The selected JSON is not a valid connection configuration.",
                "management_revision_conflict"=>"The connection changed. Cancel and refresh its details.",
                _=>"The connection could not be reviewed. Cancel and check access or recover the original saved request.",
            }));
        }
        text
    }
    pub(super) async fn accept(self) -> Result<Value, String> {
        if !self.ready() {
            return Err("web_connection_review_required".into());
        }
        match self
            .consent
            .ok_or("web_connection_review_required")?
            .accept()
            .await?
        {
            ConnectionOutcome::Applied(result) => Ok(result),
            ConnectionOutcome::Export {
                filename,
                bytes,
                lease,
                account,
                id,
            } => {
                let saved = private_export::save_named(bytes, lease, account, filename).await?;
                Ok(serde_json::json!({"id":id,"saved":saved}))
            }
        }
    }
}

#[cfg(target_os = "macos")]
async fn pick_configuration(
    lease: crate::miniapps::Lease,
    account: String,
) -> Result<Option<String>, String> {
    lease.check(&account)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    // Match Makepad's native panels. The cross-platform picker dispatches to
    // GCD again, which would put runModal inside a main-queue drain callback.
    private_export::on_main_run_loop(move || {
        let selected = std::panic::catch_unwind(|| {
            lease.check(&account)?;
            choose_configuration_path()
        })
        .unwrap_or_else(|_| Err("connection_file_unavailable".into()));
        std::thread::spawn(move || {
            let value = selected.and_then(|path| {
                lease.check(&account)?;
                path.map(|path| read_configuration(&path, || lease.check(&account)))
                    .transpose()
            });
            let _ = tx.send(value);
        });
    });
    rx.await
        .map_err(|_| "connection_file_unavailable".to_owned())?
}

#[cfg(target_os = "macos")]
fn choose_configuration_path() -> Result<Option<std::path::PathBuf>, String> {
    use makepad_apple_sys::*;
    unsafe {
        let panel: ObjcId = msg_send![class!(NSOpenPanel), openPanel];
        if panel == nil {
            return Err("connection_file_unavailable".into());
        }
        let () = msg_send![panel, setCanChooseFiles: YES];
        let () = msg_send![panel, setCanChooseDirectories: NO];
        let () = msg_send![panel, setAllowsMultipleSelection: NO];
        let response: i64 = msg_send![panel, runModal];
        match response {
            0 => Ok(None),
            1 => {
                let url: ObjcId = msg_send![panel, URL];
                if url == nil {
                    return Err("connection_file_unavailable".into());
                }
                let path: ObjcId = msg_send![url, path];
                if path == nil {
                    return Err("connection_file_unavailable".into());
                }
                Ok(Some(nsstring_to_string(path).into()))
            }
            _ => Err("connection_file_unavailable".into()),
        }
    }
}

fn read_configuration(
    path: &std::path::Path,
    authorize: impl Fn() -> Result<(), String>,
) -> Result<String, String> {
    use std::io::Read;
    authorize()?;
    let handle = std::fs::File::open(path)
        .map_err(|_| "connection_file_unavailable".to_owned())?;
    if !handle
        .metadata()
        .map_err(|_| "connection_file_unavailable".to_owned())?
        .is_file()
    {
        return Err("connection_file_unavailable".into());
    }
    let mut bytes = Vec::new();
    handle
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "connection_file_unavailable".to_owned())?;
    if bytes.len() > 65536 {
        return Err("invalid_connection_configuration".into());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| "invalid_connection_configuration".to_owned())?;
    authorize()?;
    Ok(text)
}

#[cfg(not(target_os = "macos"))]
async fn pick_configuration(
    lease: crate::miniapps::Lease,
    account: String,
) -> Result<Option<String>, String> {
    lease.check(&account)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let result = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
    let choose = move || {
        let callback_result = result.clone();
        let callback_lease = lease.clone();
        let callback_account = account.clone();
        let picked =
            move |picked: robius_file_picker::Result<Option<robius_file_picker::PickedFile>>| {
                std::thread::spawn(move || {
                    let value = (|| {
                        callback_lease.check(&callback_account)?;
                        let Some(file) =
                            picked.map_err(|_| "connection_file_unavailable".to_owned())?
                        else {
                            return Ok(None);
                        };
                        let file = file
                            .into_local_file()
                            .map_err(|_| "connection_file_unavailable".to_owned())?;
                        let text = read_configuration(file.path(), || {
                            callback_lease.check(&callback_account)
                        })?;
                        Ok(Some(text))
                    })();
                    if let Some(tx) = callback_result.lock().ok().and_then(|mut v| v.take()) {
                        let _ = tx.send(value);
                    }
                });
            };
        let started = lease.check(&account).and_then(|_| {
            robius_file_picker::FileDialog::new()
                .pick_file(picked)
                .map_err(|_| "connection_file_unavailable".into())
        });
        if let Err(error) = started {
            if let Some(tx) = result.lock().ok().and_then(|mut v| v.take()) {
                let _ = tx.send(Err(error));
            }
        }
    };
    choose();
    rx.await
        .map_err(|_| "connection_file_unavailable".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::read_configuration;

    #[test]
    fn configuration_read_enforces_bound_encoding_and_current_authority() {
        let root = std::env::temp_dir().join(format!("rinx-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("config.json");
        std::fs::write(&path, b"{}").unwrap();
        assert_eq!(read_configuration(&path, || Ok(())).unwrap(), "{}");
        let checks = std::cell::Cell::new(0);
        assert_eq!(read_configuration(&path, || {
            checks.set(checks.get() + 1);
            if checks.get() == 1 { Ok(()) } else { Err("revoked".into()) }
        }), Err("revoked".into()));
        assert_eq!(checks.get(), 2);
        std::fs::write(&path, vec![b' '; 65536]).unwrap();
        assert_eq!(read_configuration(&path, || Ok(())).unwrap().len(), 65536);
        std::fs::write(&path, vec![b' '; 65537]).unwrap();
        assert_eq!(read_configuration(&path, || Ok(())), Err("invalid_connection_configuration".into()));
        std::fs::write(&path, [0xff]).unwrap();
        assert_eq!(read_configuration(&path, || Ok(())), Err("invalid_connection_configuration".into()));
        assert!(read_configuration(&root, || Ok(())).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
