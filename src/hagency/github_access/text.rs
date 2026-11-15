//! The words of the "GitHub access" section: the how-to and the result of
//! "Check", kept apart from the widget so the mapping is tested directly.
use ::hagency::github_access::check::{CheckError, CheckReport, RepoAccess};

/// Where a fine-grained token is created.
pub const NEW_TOKEN_URL: &str = "https://github.com/settings/personal-access-tokens/new";


/// The lines "Check" shows: who the token signs in as, then each repository.
pub fn report_lines(report: &CheckReport) -> String {
    let mut lines = vec![crate::i18n::format("Signed in to GitHub as {login}.", &[("login", report.login.clone())])];
    if report.repos.is_empty() {
        lines.push(crate::i18n::tr("No repository to check yet: set one under Issue watching.").to_owned());
    }
    for (repo, access) in &report.repos {
        lines.push(format!("{repo}: {}", access_text(access)));
    }
    lines.join("\n")
}

pub fn access_text(access: &RepoAccess) -> String {
    let fixed = |text: &str| crate::i18n::tr(text).to_owned();
    match access {
        RepoAccess::Granted { push: true, .. } => fixed("push and pull ✓"),
        RepoAccess::Granted { pull: true, .. } => fixed("pull only (cannot push)"),
        RepoAccess::Granted { .. } => fixed("no access"),
        RepoAccess::NotReachable => fixed("not reachable with this token"),
        RepoAccess::Status(status) => crate::i18n::format("GitHub answered {status}", &[("status", status.to_string())]),
        RepoAccess::Http(error) => crate::i18n::format("could not reach GitHub: {error}", &[("error", error.clone())]),
    }
}

pub fn error_text(error: &CheckError) -> String {
    match error {
        CheckError::Unauthorized => crate::i18n::tr("Check failed: GitHub refused the token (401). It is wrong, expired or revoked.").to_owned(),
        CheckError::Status(status) => crate::i18n::format("Check failed: GitHub answered {status}.", &[("status", status.to_string())]),
        CheckError::Http(error) => crate::i18n::format("Check failed: could not reach GitHub: {error}", &[("error", error.clone())]),
    }
}
