//! A native folder selection never returns its path to Splash.
use rinx_miniapp_core::Lease;
use std::path::PathBuf;

pub(super) async fn choose(lease: Lease, account: String) -> Result<Option<PathBuf>, String> {
    lease.check(&account)?;
    #[cfg(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ))]
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let select = move || {
            let result = std::panic::catch_unwind(|| {
                lease.check(&account)?;
                let path = select_directory()?;
                lease.check(&account)?;
                Ok(path)
            })
            .unwrap_or_else(|_| Err("personal_directory_picker_unavailable".into()));
            let _ = tx.send(result);
        };
        #[cfg(target_os = "macos")]
        {
            use makepad_apple_sys::*;
            let select = std::sync::Mutex::new(Some(select));
            unsafe {
                let block = objc_block!(move || {
                    if let Some(select) = select.lock().unwrap().take() {
                        select();
                    }
                });
                let queue: ObjcId = msg_send![class!(NSOperationQueue), mainQueue];
                let operation: ObjcId =
                    msg_send![class!(NSBlockOperation), blockOperationWithBlock: &block];
                let () = msg_send![queue, addOperation: operation];
            }
        }
        #[cfg(not(target_os = "macos"))]
        std::thread::spawn(select);
        rx.await
            .map_err(|_| "personal_directory_picker_unavailable".to_owned())?
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    )))]
    Err("personal_directory_picker_unavailable".into())
}

#[cfg(target_os = "macos")]
fn select_directory() -> Result<Option<PathBuf>, String> {
    use makepad_apple_sys::*;
    unsafe {
        let panel: ObjcId = msg_send![class!(NSOpenPanel), openPanel];
        if panel == nil {
            return Err("personal_directory_picker_unavailable".into());
        }
        let () = msg_send![panel, setCanChooseFiles: NO];
        let () = msg_send![panel, setCanChooseDirectories: YES];
        let () = msg_send![panel, setAllowsMultipleSelection: NO];
        let () = msg_send![panel, setCanCreateDirectories: YES];
        let response: i64 = msg_send![panel, runModal];
        match response {
            0 => Ok(None),
            1 => {
                let url: ObjcId = msg_send![panel, URL];
                if url == nil {
                    return Err("personal_directory_picker_unavailable".into());
                }
                let path: ObjcId = msg_send![url, path];
                if path == nil {
                    return Err("personal_directory_picker_unavailable".into());
                }
                Ok(Some(nsstring_to_string(path).into()))
            }
            _ => Err("personal_directory_picker_unavailable".into()),
        }
    }
}
#[cfg(any(
    target_os = "windows",
    all(target_os = "linux", not(target_env = "ohos"))
))]
fn select_directory() -> Result<Option<PathBuf>, String> {
    Ok(rfd::FileDialog::new().pick_folder())
}

pub(super) const fn supported() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "windows",
        all(target_os = "linux", not(target_env = "ohos"))
    ))
}
