//! Account-scoped SDK operations. UI actions contain status, never recovery keys.
use super::pending::{self, Plan};
use crate::{matrix_context::SessionPin, sliding_sync::get_client};
use anyhow::{Context, Result, ensure};
use matrix_sdk::{
    Client,
    encryption::CrossSigningResetAuthType,
    ruma::{
        api::client::{
            keys::get_keys,
            uiaa::{AuthType, MatrixUserIdentifier, UserIdentifier},
        },
        events::GlobalAccountDataEventType,
        serde::Raw,
    },
};
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
    /// A reset is the way out when this device cannot get the account's existing keys. While
    /// the device holds them and the backup is connected, a reset would only destroy.
    pub fn can_reset(&self) -> bool {
        let exists = self.identity || self.backup || self.default_key.is_some();
        let healthy =
            self.verified && self.private_keys && (!self.backup || self.backup_connected);
        exists && !healthy
    }
}
#[derive(Clone, Debug)]
pub(super) struct Update {
    pub pin: SessionPin,
    pub request: u64,
    pub result: Result<Snapshot, String>,
    pub notice: Option<&'static str>,
}
/// A step of a running operation that waits for the person, shown while the panel stays busy.
#[derive(Clone, Debug)]
pub(super) struct Progress {
    pub pin: SessionPin,
    pub request: u64,
    pub notice: &'static str,
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
    /// Replace the account's encryption identity. Carries the account password, which the
    /// server may ask for to confirm; empty when the person gave none.
    Reset(Zeroizing<String>),
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
/// Whether the server holds this device's signature by the account's current self-signing key:
/// what other people's clients check. The local store counts the device as signed from the
/// moment the signature is made, whether or not its upload arrived.
async fn device_signed_on_server(client: &Client) -> Result<bool> {
    let user = client.user_id().context("No user")?.to_owned();
    let device = client.device_id().context("No device")?.to_owned();
    let mut request = get_keys::v3::Request::new();
    request.device_keys.insert(user.clone(), Vec::new());
    let response = client.send(request).await?;
    let Some(signing) = response.self_signing_keys.get(&user) else {
        return Ok(false);
    };
    let signing: serde_json::Value = serde_json::from_str(signing.json().get())?;
    let Some(key) = signing["keys"]
        .as_object()
        .and_then(|keys| keys.keys().next())
    else {
        return Ok(false);
    };
    let Some(own) = response
        .device_keys
        .get(&user)
        .and_then(|devices| devices.get(&device))
    else {
        return Ok(false);
    };
    let own: serde_json::Value = serde_json::from_str(own.json().get())?;
    Ok(own["signatures"][user.as_str()].get(key).is_some())
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
        && device.is_some_and(|d| d.is_cross_signed_by_owner() && !d.is_deleted())
        && device_signed_on_server(client).await?;
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
/// Makes the recovery key of a new set-up and records the public intent. Nothing is uploaded
/// before the person has saved the key.
async fn prepare(client: &Client, pin: &SessionPin) -> Result<()> {
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
    Ok(())
}
/// Replaces the account's cross-signing identity with a new one that this device holds, then
/// removes what only the old identity could use: the room key backup on the server and the
/// default secret-storage key.
///
/// The server confirms the replacement first, with the account password or on its own approval
/// page. Only a confirmed replacement goes on to delete anything, so a refused or abandoned
/// confirmation leaves the account as it was. (The SDK's own `reset_identity` deletes the backup
/// before it asks.)
async fn reset_identity(
    client: &Client,
    pin: &SessionPin,
    request: u64,
    password: &str,
) -> Result<()> {
    let encryption = client.encryption();
    let user = client.user_id().context("No user")?.to_owned();
    let before = master(&identity(client, pin).await?);
    let confirmation = match encryption.reset_cross_signing().await {
        Ok(confirmation) => confirmation,
        Err(error) => {
            makepad_widgets::warning!("encryption identity reset was not accepted: {error}");
            still_unchanged(
                client,
                pin,
                &before,
                "The server did not accept the new identity. Nothing was changed.",
            )
            .await?;
            None
        }
    };
    if let Some(confirmation) = confirmation {
        let auth = match confirmation.auth_type() {
            CrossSigningResetAuthType::Uiaa(info) => {
                ensure!(
                    info.auth_info()
                        .flows
                        .iter()
                        .any(|flow| flow.stages == [AuthType::Password]),
                    "This server confirms a reset in a way this app does not support. Nothing was changed."
                );
                ensure!(
                    !password.is_empty(),
                    "Enter your account password to confirm the reset. Nothing was changed."
                );
                info.as_password_auth_data(
                    UserIdentifier::Matrix(MatrixUserIdentifier::new(user.to_string())),
                    password.to_owned(),
                )
            }
            CrossSigningResetAuthType::OAuth(info) => {
                current(pin)?;
                robius_open::Uri::new(info.approval_url.as_str())
                    .open()
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "The server's approval page could not be opened. Nothing was changed."
                        )
                    })?;
                makepad_widgets::Cx::post_action(Progress {
                    pin: pin.clone(),
                    request,
                    notice: "Approve the reset on the page that opened in your browser. Waiting up to two minutes…",
                });
                info.as_auth_data()
            }
        };
        current(pin)?;
        if let Err(error) = confirmation.auth(auth).await {
            makepad_widgets::warning!("encryption identity reset was not confirmed: {error}");
            still_unchanged(
                client,
                pin,
                &before,
                "The server did not confirm the reset. Check your account password and try again. Nothing was changed.",
            )
            .await?;
        }
    }
    // From here on the account has the new identity.
    current(pin)?;
    encryption.backups().disable_and_delete().await.context(
        "The identity was reset, but the old room key backup could not be removed. Reset again to finish.",
    )?;
    // The SDK's marker for "no secret storage": the old recovery key protected only the old
    // identity's secrets, and a new set-up must not be refused because of it.
    client
        .account()
        .set_account_data_raw(
            GlobalAccountDataEventType::SecretStorageDefaultKey,
            Raw::new(&serde_json::json!({}))?.cast_unchecked(),
        )
        .await
        .context(
            "The identity was reset, but the old recovery key could not be retired. Reset again to finish.",
        )?;
    // The request that follows the new identity carries this device's signature. Where it did
    // not arrive, other people's clients would see an unverified device: send it once more.
    if !device_signed_on_server(client).await.unwrap_or(false)
        && let Some(device) = encryption.get_own_device().await?
    {
        current(pin)?;
        if let Err(error) = device.verify().await {
            makepad_widgets::warning!("this device's signature did not reach the server: {error}");
        }
    }
    // Asking for the identity also brings this device's record up to date with the server.
    let own = identity(client, pin).await?;
    let device = encryption.get_own_device().await?;
    ensure!(
        own.as_ref().is_some_and(|i| i.is_verified())
            && device.is_some_and(|d| d.is_cross_signed_by_owner() && !d.is_deleted())
            && encryption
                .cross_signing_status()
                .await
                .is_some_and(|s| s.is_complete())
            && device_signed_on_server(client).await.unwrap_or(false),
        UNCONFIRMED_RESET
    );
    Ok(())
}
const UNCONFIRMED_RESET: &str =
    "The reset is not confirmed. Refresh, and reset again if this device is still not verified.";
/// A step of the replacement failed. The request that carries the new identity is followed by
/// one that carries its signatures, so a failure does not say which side of the replacement the
/// account is on. Fails with `unchanged` only when the server still holds the identity it had
/// `before`; returns when the new one is already there, so that the reset goes on.
async fn still_unchanged(
    client: &Client,
    pin: &SessionPin,
    before: &Option<String>,
    unchanged: &'static str,
) -> Result<()> {
    match identity(client, pin).await {
        Ok(own) if master(&own) == *before => anyhow::bail!(unchanged),
        Ok(_) => Ok(()),
        Err(_) => anyhow::bail!(UNCONFIRMED_RESET),
    }
}
async fn perform(
    client: &Client,
    pin: &SessionPin,
    request: u64,
    op: Operation,
) -> Result<Option<&'static str>> {
    current(pin)?;
    match op {
        Operation::Refresh => {}
        Operation::Prepare => {
            prepare(client, pin).await?;
            return Ok(Some("Save your recovery key, then enable recovery."));
        }
        Operation::Reset(password) => {
            let state = snapshot(client, pin).await?;
            ensure!(
                state.can_reset(),
                "This device already holds the account's keys. Nothing was reset."
            );
            reset_identity(client, pin, request, &password).await?;
            // A recovery set-up, finished or not, belonged to the replaced identity.
            Plan::void(&root(), client)?;
            clear();
            // Straight on to a recovery key for the new identity, so that the next device
            // needs neither this one nor another reset.
            if file_picker_supported() && prepare(client, pin).await.is_ok() {
                return Ok(Some(
                    "Encryption identity reset. Save your new recovery key, then enable recovery.",
                ));
            }
            return Ok(Some("Encryption identity reset. Set up recovery next."));
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
            let notice = perform(&client, &pin, request, op).await?;
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

#[cfg(test)]
#[path = "reset_tests.rs"]
mod reset_tests;
