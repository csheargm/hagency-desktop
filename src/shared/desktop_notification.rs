//! A macOS desktop notification for things that wait for the person, such as
//! an Agent's approval request. Makepad has no notification op, and the old
//! fork's one was ignored on macOS, so an owner whose window was behind
//! another app was never told.
//!
//! The text goes to `osascript` as arguments of a fixed script, never into
//! the script source, so a command shown in a request cannot inject script.

/// Longest title and body handed to the system, in characters.
const MAX_TITLE: usize = 80;
const MAX_BODY: usize = 240;
/// Reads both arguments and shows them; the text itself is never parsed.
const SCRIPT: [&str; 3] = ["on run argv", "display notification (item 2 of argv) with title (item 1 of argv)", "end run"];

/// `text` cut to `max` characters, with an ellipsis when cut.
pub fn clip(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The `osascript` arguments that show `title` and `body`.
pub fn arguments(title: &str, body: &str) -> Vec<String> {
    let mut args = Vec::new();
    for line in SCRIPT {
        args.push("-e".to_owned());
        args.push(line.to_owned());
    }
    args.push(clip(title, MAX_TITLE));
    args.push(clip(body, MAX_BODY));
    args
}

/// Show a notification. Never blocks the UI thread; a failure is logged,
/// because the popup inside the window still shows the same text.
pub fn show(title: &str, body: &str) {
    if cfg!(test) || !cfg!(target_os = "macos") {
        return;
    }
    let args = arguments(title, body);
    std::thread::spawn(move || {
        match std::process::Command::new("/usr/bin/osascript").args(&args)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => makepad_widgets::log!("Desktop notification not shown: osascript {status}"),
            Err(error) => makepad_widgets::log!("Desktop notification not shown: {error}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_passed_as_arguments_and_clipped() {
        let args = arguments("bird-claude wants to use Bash", "git fetch origin\n\"; do shell script \"rm -rf ~\"");
        assert_eq!(&args[..6], &["-e", SCRIPT[0], "-e", SCRIPT[1], "-e", SCRIPT[2]]);
        // The request text is data after the script, never part of it.
        assert_eq!(args[7], "git fetch origin \"; do shell script \"rm -rf ~\"");
        assert!(args[..6].iter().all(|a| !a.contains("rm -rf")));
        let long = "x".repeat(500);
        assert_eq!(clip(&long, MAX_BODY).chars().count(), MAX_BODY);
        assert!(clip(&long, MAX_BODY).ends_with('…'));
        assert_eq!(clip("short", MAX_BODY), "short");
    }
}
