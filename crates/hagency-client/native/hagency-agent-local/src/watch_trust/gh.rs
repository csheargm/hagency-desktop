//! The `gh pr create|ready|edit` shapes push trust allows. Each option is
//! matched exactly (no clusters, no `--web`/`--editor`, no host), and a
//! file-reading option must name a file inside the workspace.
use crate::outside;
use std::path::Path;

/// `gh pr create` options that take a value / take none.
/// No `--fill*` and no `--template`: those read commit messages or files
/// from the git checkout gh runs in, which need not be the watched
/// repository's (integration GPT review, Nov 17); the body is explicit.
pub(crate) const CREATE_VALUED: [&str; 20] = [
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
/// Words before the options: `gh pr <verb>`.
const PR_HEAD: usize = 3;
/// Largest body file read: GitHub's body limit (65,536 characters) in UTF-8.
const MAX_PR_BODY_FILE_BYTES: u64 = 4 * 65_536;
/// Options whose value is a file `gh` reads and publishes.
const FILE_OPTIONS: [&str; 2] = ["-F", "--body-file"];

/// A parsed `gh pr <verb>` that push trust may allow.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PrCommand<'a> {
    repo: Option<&'a str>,
    /// A pull request URL target: `gh` uses its repository.
    url_repo: Option<&'a str>,
    files: Vec<&'a str>,
    /// Where each body file is named in `rest`: the option's index, and
    /// whether its value is inline (`--body-file=x`).
    file_at: Vec<(usize, bool)>,
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
        let mut words = rest.iter().enumerate();
        while let Some((at, word)) = words.next() {
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
                None => words.next()?.1,
            };
            if FILE_OPTIONS.contains(&name) {
                command.file_at.push((at, inline.is_some()));
            }
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
            // Empty is "not given" to gh, which then pushes (GPT review, Nov 18).
            self.head = super::git::branch_name(value);
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
    fn allowed(&self, root: &Path, repo: &str) -> bool {
        let named = self.url_repo.or(self.repo);
        let aimed = named.is_some_and(|named| named.eq_ignore_ascii_case(repo)) && self.repo.is_none_or(|r| r.eq_ignore_ascii_case(repo));
        aimed && self.files.iter().all(|file| *file == "-" || body_file_ok(file, root, repo))
    }

    /// The pinned line for `words` (`gh pr <verb> ...` whose options were
    /// parsed) when [`Self::allowed`]. Each body file is read once, here,
    /// and its text passed as `--body`: gh never opens the path the check
    /// saw, so a later swap to a link cannot publish another file (GPT review).
    pub(crate) fn pinned(&self, words: &[&str], root: &Path, repo: &str) -> Option<String> {
        if !self.allowed(root, repo) || words.len() < PR_HEAD {
            return None;
        }
        let mut out: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
        for &(at, inline) in &self.file_at {
            let option = PR_HEAD + at;
            let file = if inline { words[option].split_once('=')?.1 } else { *words.get(option + 1)? };
            if file == "-" {
                continue;
            }
            let body = read_body_file(file, root, repo)?;
            if inline {
                out[option] = format!("--body={body}");
            } else {
                out[option] = "--body".to_owned();
                out[option + 1] = body;
            }
        }
        let words: Vec<&str> = out.iter().map(String::as_str).collect();
        Some(pinned_command(&words))
    }
}

/// The pinned line for checked `words` (`gh pr <verb> ...`): rebuilt from
/// them, each quoted, so the shell cannot expand anything the check did not
/// see (`"$?"`, zsh's `=cmd`), on github.com whatever gh's default host is
/// (GPT reviews, Nov 18). `gh` stays bare for [`super::step_env::runs_gh`].
pub(crate) fn pinned_command(words: &[&str]) -> String {
    let rest: Vec<String> = words.iter().skip(1).map(|w| super::quote(w)).collect();
    super::pin_env(&format!("GH_HOST='github.com' gh {}", rest.join(" ")))
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

/// The text of body file `file`, read once through a descriptor that
/// is checked to be the very file [`body_file_ok`] approved: opened without
/// following a final link, a regular file with one name, the same inode as
/// the approved real path after the open (a directory swapped for a link in
/// between is caught), UTF-8, no NUL, within GitHub's size.
#[cfg(unix)]
fn read_body_file(file: &str, root: &Path, repo: &str) -> Option<String> {
    use rustix::fs::{FileType, Mode, OFlags};
    use std::io::Read;
    if !body_file_ok(file, root, repo) {
        return None;
    }
    let real = Path::new(file).canonicalize().ok()?;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::open(&real, flags, Mode::empty()).ok()?;
    let opened = rustix::fs::fstat(&fd).ok()?;
    let regular = FileType::from_raw_mode(opened.st_mode) == FileType::RegularFile;
    if !regular || opened.st_nlink != 1 || opened.st_size as u64 > MAX_PR_BODY_FILE_BYTES {
        return None;
    }
    let now = rustix::fs::lstat(&Path::new(file).canonicalize().ok()?).ok()?;
    if (now.st_dev, now.st_ino) != (opened.st_dev, opened.st_ino) || !body_file_ok(file, root, repo) {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::from(fd).take(MAX_PR_BODY_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    (text.len() as u64 <= MAX_PR_BODY_FILE_BYTES && !text.contains('\0')).then_some(text)
}

#[cfg(not(unix))]
fn read_body_file(_file: &str, _root: &Path, _repo: &str) -> Option<String> {
    None
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

#[cfg(test)]
#[path = "agent_shapes_tests.rs"]
mod agent_shapes_tests;
