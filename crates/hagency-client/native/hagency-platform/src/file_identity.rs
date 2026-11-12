//! Compare retained regular-file objects. Equality does not establish content,
//! permissions, link safety, durability, or workspace authorization.
use std::{fs::File, io};

/// A local physical observation. Serialized identity is never a file grant.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    platform: String,
    volume: String,
    object: [u8; 16],
}

pub fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "regular file required",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut object = [0; 16];
        object[8..].copy_from_slice(&metadata.ino().to_be_bytes());
        Ok(FileIdentity {
            platform: "unix-v1".into(),
            volume: format!("{:016x}", metadata.dev()),
            object,
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
        };
        let mut value = FILE_ID_INFO::default();
        // SAFETY: the borrowed regular File retains its handle through this
        // synchronous call. The initialized output has the exact Win32 size
        // and alignment and is read only after successful completion.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&mut value as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(FileIdentity {
            platform: "windows-v1".into(),
            volume: format!("{:016x}", value.VolumeSerialNumber),
            object: value.FileId.Identifier,
        })
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "regular-file identity unavailable",
    ))
}

pub fn same_file(left: &File, right: &File) -> io::Result<bool> {
    Ok(file_identity(left)? == file_identity(right)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(path: &std::path::Path) -> File {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
            options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        }
        options.open(path).unwrap()
    }

    #[test]
    fn native_receive_file_identity() {
        let root = tempfile::tempdir().unwrap();
        let original_path = root.path().join("original");
        let other_path = root.path().join("other");
        std::fs::write(&original_path, b"same bytes").unwrap();
        std::fs::write(&other_path, b"same bytes").unwrap();
        let original = File::open(&original_path).unwrap();
        assert!(same_file(&original, &original.try_clone().unwrap()).unwrap());
        let identity = file_identity(&original).unwrap();
        let serialized = serde_json::to_vec(&identity).unwrap();
        assert_eq!(
            serde_json::from_slice::<FileIdentity>(&serialized).unwrap(),
            identity
        );
        let mut other_platform: serde_json::Value = serde_json::from_slice(&serialized).unwrap();
        other_platform["platform"] = "unknown-platform-v9000".into();
        assert_ne!(
            serde_json::from_value::<FileIdentity>(other_platform.clone()).unwrap(),
            identity
        );
        other_platform["unknown"] = true.into();
        assert!(serde_json::from_value::<FileIdentity>(other_platform).is_err());
        assert!(same_file(&original, &File::open(&original_path).unwrap()).unwrap());
        assert!(!same_file(&original, &File::open(&other_path).unwrap()).unwrap());

        let alias = root.path().join("alias");
        std::fs::hard_link(&original_path, &alias).unwrap();
        assert!(same_file(&original, &File::open(&alias).unwrap()).unwrap());
        // Link-count policy belongs to the sink, not this object comparison.
        std::fs::rename(&original_path, root.path().join("moved-original")).unwrap();
        std::fs::write(&original_path, b"same bytes").unwrap();
        assert!(!same_file(&original, &File::open(&original_path).unwrap()).unwrap());
        assert!(same_file(&original, &File::open(&alias).unwrap()).unwrap());

        let directory = directory(root.path());
        for (left, right) in [(&original, &directory), (&directory, &original)] {
            assert_eq!(
                same_file(left, right).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
