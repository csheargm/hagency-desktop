//! The `gh pr create|ready|edit` shapes push trust allows. Each option is
//! matched exactly (no clusters, no `--web`/`--editor`, no host), and a
//! file-reading option must name a file inside the workspace.
use crate::outside;
use std::path::Path;

/// `gh pr create` options that take a value / take none.
/// No `--fill*` and no `--template`: those read commit messages or files
/// from the git checkout gh runs in, which need not be the watched
/// repository's (integration GPT review, Nov 17); the body is explicit.
const CREATE_VALUED: [&str; 20] = [
    "-a", "--assignee", "-B", "--base", "-b", "--body", "-H", "--head", "-l", "--label", "-m", "--milestone",
    "-p", "--project", "-r", "--reviewer", "-t", "--title", "-F", "--body-file",
];
const CREATE_FLAGS: [&str; 3] = ["-d", "--draft", "--no-maintainer-edit"];
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
    /// `create` names `--head`, so gh never pushes the branch itself.
    head: bool,
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
        // Without --head, `gh pr create` may push the branch with the
        // checkout's git config (GPT review): only the pinned push publishes.
        if positional.len() > targets || (verb == "create" && !command.head) {
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
        } else if matches!(name, "-H" | "--head") {
            self.head = !value.contains(':') && !value.starts_with('-');
        } else if FILE_OPTIONS.contains(&name) {
            self.files.push(value);
        }
        Some(())
    }

    /// Names `repo` (never left to the checkout's remotes or the shell's
    /// directory), and every body file is `-` (stdin) or an absolute path
    /// to a regular file inside `repo`'s own checkout in the workspace,
    /// never git metadata (named or resolved): a PR body publishes the
    /// file, and push freely now does so even under Deny (GPT review, Nov 17).
    pub(crate) fn allowed(&self, root: &Path, repo: &str) -> bool {
        let named = self.url_repo.or(self.repo);
        let aimed = named.is_some_and(|named| named.eq_ignore_ascii_case(repo)) && self.repo.is_none_or(|r| r.eq_ignore_ascii_case(repo));
        aimed && self.files.iter().all(|file| *file == "-" || body_file_ok(file, root, repo))
    }
}

/// A body file push trust may publish: see [`PrCommand::allowed`].
fn body_file_ok(file: &str, root: &Path, repo: &str) -> bool {
    let roots = [root.to_path_buf()];
    if !Path::new(file).is_absolute() || !outside::inside_all(file, &roots, root) || outside::is_git_metadata(Path::new(file)) {
        return false;
    }
    // An existing regular file, through every link.
    let Some(real) = Path::new(file).canonicalize().ok().filter(|r| !outside::is_git_metadata(r) && r.is_file()) else { return false };
    super::local_git::checkout_containing(&real, root).is_some_and(|checkout| super::local_git::own_checkout(&checkout, repo))
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
