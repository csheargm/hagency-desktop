//! Account-scoped SDK operations. UI actions contain status, never recovery keys.
use super::pending::{self, Plan};
use crate::{matrix_context::SessionPin, sliding_sync::get_client};
use anyhow::{Context, Result, ensure};
use matrix_sdk::Client;
use matrix_sdk_base::crypto::secret_storage::SecretStorageKey;
use std::sync::Mutex;
use zeroize::Zeroizing;

static OPERATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct SavedKey {
    pin: SessionPin,
    key: Zeroizing<String>,
    saved: bool,
}
static KEY: Mutex<Option<SavedKey>> = Mutex::new(None);
#[derive(Clone, Debug, Default)]
pub(super) struct Snapshot {
    pub identity: bool,
    pub verified: bool,
    pub private_keys: bool,
    pub default_key: Option<String>,
    pub backup: bool,
    pub backup_connected: bool,
    pub pending: bool,
    pub applying: bool,
    pub key_loaded: bool,
    pub key_saved: bool,
}
impl Snapshot {
    pub fn can_prepare(&self) -> bool {
        !self.pending
            && self.default_key.is_none()
            && (!self.identity || self.private_keys)
            && (!self.backup || self.backup_connected)
    }
}
#[derive(Clone, Debug)]
pub(super) struct Update {
    pub pin: SessionPin,
    pub request: u64,
    pub result: Result<Snapshot, String>,
    pub notice: Option<&'static str>,
}
pub(super) enum Operation {
    Refresh,
    Prepare,
    Discard,
    Save,
    LoadPending,
    Enable,
    RestoreFile,
    Restore(Zeroizing<String>),
    Upload,
}
pub(super) fn clear() {
    KEY.lock().unwrap().take();
}
fn root() -> std::path::PathBuf {
    crate::app_data_dir().join("encryption-recovery")
}
fn current(pin: &SessionPin) -> Result<Client> {
    ensure!(
        pin.is_current(),
        "The Matrix session changed. Reopen this panel."
    );
    get_client().context("Sign in first")
}
async fn identity(
    client: &Client,
    pin: &SessionPin,
) -> Result<Option<matrix_sdk::encryption::identities::UserIdentity>> {
    current(pin)?;
    let own = client
        .encryption()
        .request_user_identity(client.user_id().context("No user")?)
        .await?;
    current(pin)?;
    Ok(own)
}
fn master(identity: &Option<matrix_sdk::encryption::identities::UserIdentity>) -> Option<String> {
    identity
        .as_ref()
        .and_then(|i| i.master_key().get_first_key())
        .map(|k| k.to_base64())
}
async fn default_key(client: &Client) -> Result<Option<String>> {
    let raw = client
        .encryption()
        .secret_storage()
        .fetch_default_key_id()
        .await?;
    let Some(raw) = raw else { return Ok(None) };
    // {} is the SDK's explicit deletion marker. Other malformed responses must
    // not be interpreted as permission to replace existing secret storage.
    if raw.json().get().trim() == "{}" {
        return Ok(None);
    }
    let value = raw
        .deserialize()
        .context("Invalid secret-storage state; no setup changes were made")?;
    ensure!(!value.key_id.is_empty(), "Invalid secret-storage key ID");
    Ok(Some(value.key_id))
}
async fn snapshot(client: &Client, pin: &SessionPin) -> Result<Snapshot> {
    let own = identity(client, pin).await?;
    let default_key = default_key(client).await?;
    let backup = client
        .encryption()
        .backups()
        .fetch_exists_on_server()
        .await?;
    let backup_connected = client.encryption().backups().are_enabled().await;
    let private_keys = client
        .encryption()
        .cross_signing_status()
        .await
        .is_some_and(|s| s.is_complete());
    let device = client.encryption().get_own_device().await?;
    let verified = own.as_ref().is_some_and(|i| i.is_verified())
        && device.is_some_and(|d| d.is_cross_signed_by_owner() && !d.is_deleted());
    let plan = Plan::load(&root(), client)?;
    let pending = plan.as_ref().is_some_and(|p| !p.complete);
    let applying = plan.is_some_and(|p| p.applying && !p.complete);
    current(pin)?;
    let keys = KEY.lock().unwrap();
    let key = keys.as_ref().filter(|k| k.pin == *pin);
    Ok(Snapshot {
        identity: own.is_some(),
        verified,
        private_keys,
        default_key,
        backup,
        backup_connected,
        pending,
        applying,
        key_loaded: key.is_some(),
        key_saved: key.is_some_and(|k| k.saved),
    })
}
fn saved_key(pin: &SessionPin, require_saved: bool) -> Result<Zeroizing<String>> {
    let keys = KEY.lock().unwrap();
    let key = keys
        .as_ref()
        .filter(|k| k.pin == *pin)
        .context("Load the recovery key you saved for this setup")?;
    ensure!(
        !require_saved || key.saved,
        "Save the recovery key before enabling recovery"
    );
    Ok(Zeroizing::new(key.key.to_string()))
}
pub(super) fn file_picker_supported() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ))
}
#[cfg(target_os = "macos")]
async fn pick_path(save: bool) -> Result<Option<std::path::PathBuf>> {
    use makepad_apple_sys::*;
    static DIALOG: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
    let permit = DIALOG
        .try_acquire()
        .context("Close the previous key dialog first")?;
    let client = get_client().context("Sign in first")?;
    let pin = SessionPin::capture(&client).context("No Matrix session")?;
    let (send, receive) = tokio::sync::oneshot::channel();
    let callback = std::sync::Mutex::new(Some(move || {
        let _original_dialog = permit;
        let chosen = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !pin.is_current() {
                return None;
            }
            let picker = rfd::FileDialog::new().set_title(if save {
                "Save your recovery key privately"
            } else {
                "Choose your recovery key file"
            });
            if save {
                picker.set_file_name("hagency-recovery-key.txt").save_file()
            } else {
                picker.pick_file()
            }
        }))
        .map_err(|_| anyhow::anyhow!("The system key dialog could not be opened"));
        let _ = send.send(chosen);
    }));
    // Makepad drives AppKit's event loop itself. Async rfd checks isRunning(),
    // which is false here. Run a synchronous panel on the main operation queue,
    // outside a dispatch callback, and return only the chosen path to the worker.
    unsafe {
        let block = objc_block!(move || {
            if let Some(choose) = callback.lock().unwrap().take() {
                choose();
            }
        });
        let queue: ObjcId = msg_send![class!(NSOperationQueue), mainQueue];
        let operation: ObjcId = msg_send![class!(NSBlockOperation),blockOperationWithBlock:&block];
        let (): () = msg_send![queue,addOperation:operation];
    }
    receive.await.context("The key dialog was interrupted")?
}
#[cfg(any(
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
async fn pick_path(save: bool) -> Result<Option<std::path::PathBuf>> {
    let picker = rfd::AsyncFileDialog::new().set_title(if save {
        "Save your recovery key privately"
    } else {
        "Choose your recovery key file"
    });
    let picked = if save {
        picker
            .set_file_name("hagency-recovery-key.txt")
            .save_file()
            .await
    } else {
        picker.pick_file().await
    };
    Ok(picked.map(|f| f.path().to_owned()))
}
#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
)))]
async fn pick_path(_save: bool) -> Result<Option<std::path::PathBuf>> {
    anyhow::bail!(
        "File-based recovery setup is available in the desktop app. Paste an existing recovery key to restore this device."
    )
}
async fn pick_key() -> Result<Option<Zeroizing<String>>> {
    let Some(path) = pick_path(false).await? else {
        return Ok(None);
    };
    Ok(Some(pending::load_key(&path)?))
}

async fn recover(client: &Client, key: &str) -> Result<()> {
    tokio::time::timeout(
        std::time::Duration::from_secs(45),
        client.encryption().recovery().recover(key),
    )
    .await
    .context(
        "Recovery is unconfirmed. Keep your saved key, refresh, and resume the original setup.",
    )??;
    Ok(())
}
async fn perform(client: &Client, pin: &SessionPin, op: Operation) -> Result<Option<&'static str>> {
    current(pin)?;
    match op {
        Operation::Refresh => {}
        Operation::Prepare => {
            ensure!(
                file_picker_supported(),
                "Set up recovery in the desktop app first"
            );
            let state = snapshot(client, pin).await?;
            ensure!(
                state.can_prepare(),
                "Restore existing recovery, or verify from an existing device before setup"
            );
            let own = identity(client, pin).await?;
            let key = SecretStorageKey::new();
            let plan = Plan::new(client, &key, master(&own))?;
            current(pin)?;
            plan.save(&root())?;
            *KEY.lock().unwrap() = Some(SavedKey {
                pin: pin.clone(),
                key: Zeroizing::new(key.to_base58()),
                saved: false,
            });
            return Ok(Some("Save your recovery key, then enable recovery."));
        }
        Operation::Discard => {
            let plan = Plan::load(&root(), client)?.context("No pending setup")?;
            ensure!(
                !plan.applying && !plan.complete,
                "Resume the original saved key; this setup may already be published"
            );
            ensure!(
                default_key(client).await?.is_none(),
                "Recovery is already configured; restore its key instead"
            );
            current(pin)?;
            plan.discard(&root())?;
            clear();
            return Ok(Some(
                "Unstarted recovery setup discarded. No server keys were changed.",
            ));
        }
        Operation::Save => {
            let key = saved_key(pin, false)?;
            let Some(path) = pick_path(true).await? else {
                return Ok(None);
            };
            current(pin)?;
            pending::save_key(&path, &key)?;
            let mut keys = KEY.lock().unwrap();
            if let Some(k) = keys.as_mut().filter(|k| k.pin == *pin) {
                k.saved = true;
            }
            return Ok(Some(
                "Recovery key saved. Keep it private and outside this device.",
            ));
        }
        Operation::LoadPending => {
            let Some(key) = pick_key().await? else {
                return Ok(None);
            };
            current(pin)?;
            let plan = Plan::load(&root(), client)?.context("No pending setup")?;
            let _ = plan.restore_key(&key)?;
            *KEY.lock().unwrap() = Some(SavedKey {
                pin: pin.clone(),
                key,
                saved: true,
            });
            return Ok(Some("Saved key checked. Continue the original setup."));
        }
        Operation::Enable => {
            let saved = saved_key(pin, true)?;
            let mut plan = Plan::load(&root(), client)?.context("No pending setup")?;
            ensure!(!plan.complete, "Recovery setup already completed");
            let key = plan.restore_key(&saved)?;
            let default = default_key(client).await?;
            ensure!(
                default.is_none() || default.as_deref() == Some(&plan.key_id),
                "Recovery settings changed on another device. Restore the current recovery key instead."
            );
            let own = identity(client, pin).await?;
            if let Some(expected) = &plan.master {
                ensure!(
                    master(&own).as_ref() == Some(expected),
                    "Your identity changed. Setup stopped without resetting it."
                );
            }
            current(pin)?;
            plan.applying = true;
            plan.save(&root())?;
            if default.is_none() {
                if own.is_none() {
                    client.encryption().bootstrap_cross_signing(None).await?;
                }
                let own = identity(client, pin).await?;
                ensure!(
                    own.as_ref().is_some_and(|i| i.is_verified())
                        && client
                            .encryption()
                            .cross_signing_status()
                            .await
                            .is_some_and(|s| s.is_complete()),
                    "Verify this device or restore existing identity keys first"
                );
                plan.master = master(&own);
                current(pin)?;
                plan.save(&root())?;
                // Re-read immediately before upload. Never reset an existing store.
                ensure!(
                    default_key(client).await?.is_none(),
                    "Recovery was enabled on another device; restore its key instead"
                );
                current(pin)?;
                let returned = Zeroizing::new(
                    client
                        .encryption()
                        .recovery()
                        .enable()
                        .with_key(&key)
                        .await?,
                );
                ensure!(*returned == *saved, "Recovery key confirmation failed");
            }
            current(pin)?;
            recover(client, &saved).await?;
            let state = snapshot(client, pin).await?;
            ensure!(
                state.default_key.as_deref() == Some(&plan.key_id)
                    && state.private_keys
                    && state.backup_connected,
                "Recovery is not fully confirmed. Keep the saved key and resume setup."
            );
            current(pin)?;
            plan.complete = true;
            plan.save(&root())?;
            clear();
            return Ok(Some(
                "Recovery enabled. Room keys are backed up as they become available.",
            ));
        }
        Operation::RestoreFile => {
            let Some(key) = pick_key().await? else {
                return Ok(None);
            };
            current(pin)?;
            recover(client, &key).await?;
            return Ok(Some(
                "Recovery key accepted. Available room keys will be downloaded as needed.",
            ));
        }
        Operation::Restore(key) => {
            ensure!(!key.trim().is_empty(), "Enter your recovery key");
            recover(client, key.trim()).await?;
            return Ok(Some(
                "Recovery key accepted. Available room keys will be downloaded as needed.",
            ));
        }
        Operation::Upload => {
            ensure!(
                client.encryption().backups().are_enabled().await,
                "Restore or enable recovery first"
            );
            client
                .encryption()
                .backups()
                .wait_for_steady_state()
                .await?;
            return Ok(Some("Currently known room keys have been uploaded."));
        }
    }
    Ok(None)
}
pub(super) fn launch(pin: SessionPin, request: u64, op: Operation) {
    crate::account_session::spawn(async move {
        let _guard = OPERATION.lock().await;
        let outcome = async {
            let client = current(&pin)?;
            let notice = perform(&client, &pin, op).await?;
            let state = snapshot(&client, &pin).await?;
            Ok::<_, anyhow::Error>((state, notice))
        }
        .await;
        if pin.is_current() {
            let (result, notice) = match outcome {
                Ok((s, n)) => (Ok(s), n),
                Err(e) => (Err(e.to_string()), None),
            };
            makepad_widgets::Cx::post_action(Update {
                pin,
                request,
                result,
                notice,
            });
        }
    });
}
