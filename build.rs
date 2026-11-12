fn main() {
    println!("cargo:rerun-if-changed=system-apps.json");
    let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let catalog = rinx_system_apps::pack(&root).expect("validate Rinx system apps");
    // The catalog has already validated these paths. Watch whole bundle
    // directories so added/deleted assets retrigger packaging, without watching
    // unrelated sources or build outputs elsewhere under the apps directory.
    let config: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("system-apps.json")).expect("read system app inputs")
    ).expect("parse validated system app inputs");
    let source = root.join(config["source"].as_str().expect("validated source"));
    for app in &catalog.apps {
        let bundle = source.join(&app.directory).join("bundle");
        println!("cargo:rerun-if-changed={}", bundle.join("manifest.json").display());
        println!("cargo:rerun-if-changed={}", bundle.display());
    }
    let catalog_path = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("system-apps.json");
    std::fs::write(catalog_path, serde_json::to_vec(&catalog).unwrap()).expect("embed Rinx system apps");

    // cargo-makepad discovers resources of Git/registry dependencies through
    // a package-name.path file beside the compiled libraries. Rinx is also a
    // hosted dependency, so publish its resource root like makepad-widgets.
    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let profile_dir = out_dir.ancestors().nth(3).unwrap();
    std::fs::write(
        profile_dir.join("rinx.path"),
        std::env::var("CARGO_MANIFEST_DIR").unwrap(),
    )
    .expect("write Rinx resource path for cargo-makepad");

    // Detect `cargo packager` builds using the `CARGO_PACKAGER_FORMAT` env var.
    println!("cargo::rustc-check-cfg=cfg(packaging_build)");
    println!("cargo:rerun-if-env-changed=CARGO_PACKAGER_FORMAT");
    if std::env::var_os("CARGO_PACKAGER_FORMAT").is_some() {
        println!("cargo:rustc-cfg=packaging_build");
    }

    // Note: `#[cfg(windows)]` checks the *host* OS, not the *target*.
    // We must check the target env at runtime to avoid running this
    // when cross-compiling (e.g., building for Android on a Windows CI runner).
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // A macOS bundle keeps its resources in `Contents/Resources`, one folder away from the
    // executable in `Contents/MacOS`. The packaging tool passes `MAKEPAD_PACKAGE_DIR=.` for a
    // bundle and leaves finding `Contents/Resources` to a bundle switch of the earlier engine
    // (`MAKEPAD=apple_bundle`). This engine does not read that switch: it looks in the package
    // directory, relative to the executable. Left at `.`, a packaged macOS app finds neither
    // its images nor its bundled fonts on any computer but the one that built it.
    if target_os == "macos"
        && matches!(std::env::var("CARGO_PACKAGER_FORMAT").as_deref(), Ok("app" | "dmg"))
    {
        println!("cargo:rustc-env=MAKEPAD_PACKAGE_DIR=../Resources");
    }

    // On macOS, we can run Rinx as an ad-hoc codesigned app,
    // which allows certain system features to work (speech recognition, location, etc).
    // See also the `.cargo/config.toml` file that overrides what `cargo run` does.
    if target_os == "macos" {
        let plist = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("packaging/macos/Info.plist");
        println!("cargo:rerun-if-changed=packaging/macos/Info.plist");
        println!("cargo:rustc-link-arg-bin=rinx=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
    }

    if target_os == "windows" {
        println!("cargo:rerun-if-changed=resources/icon.ico");
        #[cfg(windows)]
        {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("resources/icon.ico");
            // Explicit VERSIONINFO fields. Without these, Windows shows
            // "Unknown publisher" in the UAC/SmartScreen install prompt
            // (CompanyName/LegalCopyright are empty by default), and the
            // ProductName/FileDescription fall back to the lowercase crate
            // name "rinx" instead of the product name.
            res.set("CompanyName", "Hagency");
            res.set("ProductName", "Hagency Desktop");
            res.set("FileDescription", "Hagency Desktop - chat and AI Agents");
            res.set("LegalCopyright", "Copyright - 2023-2026 Project Robius");
            res.compile().expect("Failed to compile Windows resources");
        }
    }

    // Get version info about Rinx, the matrix SDK, and testflight.
    println!("cargo:rerun-if-changed=Cargo.lock");
    let (sdk_version, sdk_git_rev, sdk_url) = read_matrix_sdk_info();
    println!("cargo:rustc-env=MATRIX_SDK_VERSION={sdk_version}");
    println!("cargo:rustc-env=MATRIX_SDK_GIT_REV={sdk_git_rev}");
    println!("cargo:rustc-env=MATRIX_SDK_URL={sdk_url}");

    let (rinx_git_rev, rinx_url) = read_rinx_git_info();
    println!("cargo:rustc-env=RINX_GIT_COMMIT_HASH={rinx_git_rev}");
    println!("cargo:rustc-env=RINX_GIT_COMMIT_URL={rinx_url}");

    println!("cargo:rerun-if-env-changed=TESTFLIGHT_BUILD_NUMBER");
    let testflight_build = std::env::var("TESTFLIGHT_BUILD_NUMBER").unwrap_or_default();
    println!("cargo:rustc-env=TESTFLIGHT_BUILD_NUMBER={testflight_build}");
}

/// Returns Rinx's own current git commit info as a commit hash and a permalink.
fn read_rinx_git_info() -> (String, String) {
    // Source bundles may have no Git metadata. Watching a missing path makes
    // Cargo rerun this script (and recompile the app) on every invocation.
    if std::path::Path::new(".git/HEAD").is_file() {
        println!("cargo:rerun-if-changed=.git/HEAD");
    }
    if let Ok(head) = std::fs::read_to_string(".git/HEAD") {
        if let Some(branch_ref) = head.trim().strip_prefix("ref: ") {
            let path = format!(".git/{branch_ref}");
            if std::path::Path::new(&path).is_file() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
    if std::path::Path::new(".git/packed-refs").is_file() {
        println!("cargo:rerun-if-changed=.git/packed-refs");
    }

    let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
    else {
        return (String::new(), String::new());
    };
    if !output.status.success() {
        return (String::new(), String::new());
    }
    let full_sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if full_sha.len() < 8 {
        return (String::new(), String::new());
    }
    let short_rev: String = full_sha.chars().take(8).collect();
    let url = format!("https://github.com/hagency-org/hagency-desktop/tree/{full_sha}");
    (short_rev, url)
}

/// Parses Cargo.lock to find the resolved version of `matrix-sdk`.
///
/// Returns `(version, short_git_rev, url)`.
fn read_matrix_sdk_info() -> (String, String, String) {
    let Ok(lockfile_text) = std::fs::read_to_string("Cargo.lock") else {
        return (String::new(), String::new(), String::new());
    };
    let Ok(lockfile) = toml::from_str::<toml::Value>(&lockfile_text) else {
        return (String::new(), String::new(), String::new());
    };

    let Some(pkg) = lockfile
        .get("package")
        .and_then(|p| p.as_array())
        .and_then(|pkgs| {
            pkgs.iter().find(|p| {
                p.get("name").and_then(|n| n.as_str()) == Some("matrix-sdk")
            })
        })
    else {
        return (String::new(), String::new(), String::new());
    };

    let version = pkg
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let source = pkg.get("source").and_then(|s| s.as_str()).unwrap_or("");

    // Git sources look like `git+<repo-url>?<query>#<full-commit>`.
    // The repo URL is the prefix before `?` or `#`; the commit is after `#`.
    let (git_rev, url) = if let Some(rest) = source.strip_prefix("git+") {
        let (left, full_commit) = rest.rsplit_once('#').unwrap_or((rest, ""));
        let base = left.split_once('?').map_or(left, |(b, _)| b);
        let short_rev: String = full_commit.chars().take(8).collect();
        let url = if full_commit.is_empty() {
            base.to_string()
        } else {
            format!("{base}/tree/{full_commit}")
        };
        (short_rev, url)
    } else if !version.is_empty() {
        // Registry/path/other sources: fall back to the crates.io URL.
        (String::new(), format!("https://crates.io/crates/matrix-sdk/{version}"))
    } else {
        (String::new(), String::new())
    };

    (version, git_rev, url)
}
