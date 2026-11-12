// this stub is necessary because some platforms require building
// as dll (mobile / wasm) and some require to be built as executable
// unfortunately cargo doesn't facilitate this without a main.rs stub

// Hide the command prompt console window on Windows, if desired.
// TODO: move this into Makepad itself as an addition to the `MAKEPAD` env var.
#![cfg_attr(
    all(
        any(feature = "hide_windows_console", packaging_build),
        target_os = "windows"
    ),
    windows_subsystem = "windows"
)]

#[cfg(feature = "standalone")]
fn main() {
    if let Some(result) = hagency::native_owner::run_guardian_if_requested() {
        if result.is_err() {
            std::process::exit(1);
        }
        return;
    }
    // The per-user owner daemon runs from this binary (ADR-192): the
    // LaunchAgent starts `<this executable> daemon run --home <home>`, and
    // Desktop's hosting setup runs `daemon enroll` and `daemon install`.
    // It never starts the UI.
    if let Some(status) = hagency::daemon::cli::run_if_requested() {
        std::process::exit(status);
    }
    #[cfg(feature = "onboarding-test-support")]
    {
        let args: Vec<_> = std::env::args_os().collect();
        let roots: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[0] == "--hagency-isolated-installation-root")
            .map(|pair| std::path::PathBuf::from(&pair[1]))
            .collect();
        if roots.len() != 1
            || !roots[0].is_absolute()
            || hagency::native_owner::initialize_resource_activation_fixture(&roots[0]).is_err()
        {
            eprintln!("isolated_onboarding_root_required_before_startup");
            std::process::exit(2);
        }
    }
    // The HAL needs the main thread for its main loop; `run_app` gives it
    // that and runs the app on a thread of its own (under `makepad run`, the
    // host already did, and the app runs directly).
    rinx::makepad_widgets::run_app(rinx::app::app_main)
}

/// Without `standalone`, Rinx is a library for a host such as OctoSense.
#[cfg(not(feature = "standalone"))]
fn main() {}
