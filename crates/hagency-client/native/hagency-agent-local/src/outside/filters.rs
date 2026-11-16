//! Pure text filters a read may pipe into (`gh api ... -q .content | base64 -d
//! | grep ...`), and the `gh api` reads outside mode allows. Every option is
//! matched exactly: anything that writes a file (`sort -o`, `base64 -o`), runs
//! a program (`sort --compress-program`) or reads a file named by an option
//! (`jq --rawfile`, `-f`, `-L`) asks. Positional file names are path-checked
//! by the caller like every other read.

/// Filter programs, beyond `outside::rules`' plain reads.
pub(super) const FILTERS: [&str; 4] = ["base64", "sort", "cut", "jq"];
const BASE64_FLAGS: [&str; 3] = ["-d", "-D", "--decode"];
const SORT_FLAGS: [&str; 12] = ["-n", "-r", "-u", "-f", "-b", "-h", "-V", "-nr", "-rn", "--numeric-sort", "--reverse", "--unique"];
const JQ_FLAGS: [&str; 15] = ["-r", "--raw-output", "-c", "--compact-output", "-e", "--exit-status", "-s", "--slurp", "-n", "--null-input", "-j", "--join-output", "-S", "--sort-keys", "--tab"];
/// jq builtins that load modules or files on top of the environment reads
/// `rules` already refuses.
const JQ_DENIED: [&str; 2] = ["import", "include"];

/// A filter segment whose options are all known not to write or run anything.
pub(super) fn filter_allowed(program: &str, rest: &[&str]) -> bool {
    let options = rest.iter().filter(|w| w.starts_with('-'));
    match program {
        "base64" => options.clone().all(|w| BASE64_FLAGS.contains(w)),
        "sort" => options.clone().all(|w| SORT_FLAGS.contains(w) || sort_key(w)),
        "cut" => options.clone().all(|w| cut_option(w)),
        "jq" => options.clone().all(|w| JQ_FLAGS.contains(w)) && jq_filter(rest).is_some_and(|f| !JQ_DENIED.iter().any(|d| f.contains(d))),
        _ => false,
    }
}

/// `-k2`, `-k2,2`, `-t,`: the key and separator forms with the value attached.
fn sort_key(word: &str) -> bool {
    word.strip_prefix("-k").is_some_and(|key| !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit() || b == b',' || b == b'.'))
        || word.strip_prefix("-t").is_some_and(|sep| sep.chars().count() == 1)
}

/// `-f1`, `-f1,3`, `-c1-5`, `-b2`, `-d,` (value attached), `-s`.
fn cut_option(word: &str) -> bool {
    let list = |l: &str| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit() || b == b',' || b == b'-');
    word == "-s"
        || ["-f", "-c", "-b"].iter().any(|p| word.strip_prefix(p).is_some_and(list))
        || word.strip_prefix("-d").is_some_and(|sep| sep.chars().count() == 1)
}

/// jq's filter: its first positional word (the rest are files).
pub(super) fn jq_filter<'a>(rest: &[&'a str]) -> Option<&'a str> {
    rest.iter().copied().find(|w| !w.starts_with('-'))
}

/// Repository sub-resources a `gh api` GET may read. `actions` is left out
/// (job logs can carry secrets a job printed), as is everything outside a repository.
const GH_API_READS: [&str; 9] = ["contents", "pulls", "issues", "commits", "compare", "branches", "git", "readme", "labels"];
const GH_API_FLAGS: [&str; 4] = ["--paginate", "--slurp", "-i", "--include"];
const GH_API_VALUED: [&str; 4] = ["-q", "--jq", "-t", "--template"];

/// `gh api repos/<owner>/<name>/<read>[/...][?query] [-q filter] [-X GET]`:
/// a GET only (no fields, input, headers or other methods) of one repository resource.
pub(super) fn gh_api_read(rest: &[&str]) -> bool {
    let mut endpoint = None;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        let (name, inline) = match word.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value)),
            _ => (*word, None),
        };
        let ok = match name {
            _ if !word.starts_with('-') => endpoint.replace(*word).is_none(),
            "-X" | "--method" => inline.or_else(|| words.next().copied()).is_some_and(|m| m.eq_ignore_ascii_case("GET")),
            _ if GH_API_VALUED.contains(&name) => inline.is_some() || words.next().is_some(),
            _ => GH_API_FLAGS.contains(&name) && inline.is_none(),
        };
        if !ok {
            return false;
        }
    }
    endpoint.is_some_and(repository_read)
}

fn repository_read(endpoint: &str) -> bool {
    let path = endpoint.strip_prefix('/').unwrap_or(endpoint);
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    let parts: Vec<&str> = path.split('/').collect();
    let plain = |p: &str| !p.is_empty() && p != ".." && p != "." && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    !endpoint.contains(':')
        && matches!(parts.as_slice(), ["repos", owner, name, resource, ..] if plain(owner) && plain(name) && GH_API_READS.contains(resource))
        && parts.iter().all(|p| plain(p))
}
