//! The `gh pr create|ready|edit` shapes push trust allows. Each option is
//! matched exactly (no clusters, no `--web`/`--editor`, no host), and a
//! file-reading option must name a file inside the workspace.
use crate::outside;
use std::path::{Path, PathBuf};

/// `gh pr create` options that take a value / take none.
const CREATE_VALUED: [&str; 22] = [
    "-a", "--assignee", "-B", "--base", "-b", "--body", "-H", "--head", "-l", "--label", "-m", "--milestone",
    "-p", "--project", "-r", "--reviewer", "-t", "--title", "-T", "--template", "-F", "--body-file",
];
const CREATE_FLAGS: [&str; 7] = ["-d", "--draft", "-f", "--fill", "--fill-first", "--fill-verbose", "--no-maintainer-edit"];
/// `gh pr edit` options that take a value / take none.
const EDIT_VALUED: [&str; 18] = [
    "--add-assignee", "--add-label", "--add-project", "--add-reviewer", "-B", "--base", "-b", "--body", "-F",
    "--body-file", "-m", "--milestone", "--remove-assignee", "--remove-label", "--remove-project",
    "--remove-reviewer", "-t", "--title",
];
const EDIT_FLAGS: [&str; 1] = ["--remove-milestone"];
const READY_FLAGS: [&str; 1] = ["--undo"];
/// Options whose value is a file `gh` reads and publishes.
const FILE_OPTIONS: [&str; 2] = ["-F", "--body-file"];

/// A parsed `gh pr <verb>` that push trust may allow.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PrCommand<'a> {
    repo: Option<&'a str>,
    /// A pull request URL target: `gh` uses its repository.
    url_repo: Option<&'a str>,
    files: Vec<&'a str>,
}

impl<'a> PrCommand<'a> {
    pub(crate) fn parse(verb: &str, rest: &[&'a str]) -> Option<Self> {
        let (valued, flags, targets): (&[&str], &[&str], usize) = match verb {
            "create" => (&CREATE_VALUED, &CREATE_FLAGS, 0),
            "edit" => (&EDIT_VALUED, &EDIT_FLAGS, 1),
            "ready" => (&[], &READY_FLAGS, 1),
            _ => return None,
        };
        let mut command = Self::default();
        let mut positional = Vec::new();
        let mut words = rest.iter();
        while let Some(word) = words.next() {
            let (name, inline) = match word.split_once('=') {
                Some((name, value)) if name.starts_with("--") => (name, Some(value)),
                _ => (*word, None),
            };
            if !word.starts_with('-') {
                positional.push(*word);
                continue;
            }
            if flags.contains(&name) && inline.is_none() {
                continue;
            }
            if !matches!(name, "-R" | "--repo") && !valued.contains(&name) {
                return None;
            }
            let value = match inline {
                Some(value) => value,
                None => words.next()?,
            };
            command.take(name, value)?;
        }
        if positional.len() > targets {
            return None;
        }
        if let Some(target) = positional.first() {
            command.url_repo = target_repo(target)?;
        }
        Some(command)
    }

    fn take(&mut self, name: &str, value: &'a str) -> Option<()> {
        if matches!(name, "-R" | "--repo") {
            self.repo = Some(crate::merge_policy::gate::repo_name(value)?);
        } else if FILE_OPTIONS.contains(&name) {
            self.files.push(value);
        }
        Some(())
    }

    /// Aimed at `repo` from every directory the line may be in, and every
    /// body file stays in the workspace (`-` is stdin).
    pub(crate) fn allowed(&self, dirs: &[PathBuf], root: &Path, repo: &str) -> bool {
        let named = self.url_repo.or(self.repo);
        let aimed = match named {
            Some(named) => named.eq_ignore_ascii_case(repo) && self.repo.is_none_or(|r| r.eq_ignore_ascii_case(repo)),
            None => dirs.iter().all(|dir| super::git::all_remotes_are(dir, repo)),
        };
        aimed && self.files.iter().all(|file| *file == "-" || outside::inside_all(file, dirs, root))
    }
}

/// A target that is a number or branch (`None`: the checkout decides), or a
/// `https://github.com/<owner>/<name>/pull/<N>` URL (its repository).
/// `Some(None)` = no repository named; `None` = refused.
fn target_repo(target: &str) -> Option<Option<&str>> {
    let Some(rest) = target.strip_prefix("https://github.com/") else {
        return (!target.contains(':') && !target.starts_with('-')).then_some(None);
    };
    let (repo, number) = rest.rsplit_once("/pull/")?;
    let number_ok = !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit());
    number_ok.then(|| crate::merge_policy::gate::repo_name(repo)).flatten().map(Some)
}
