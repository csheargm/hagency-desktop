use super::*;
use crate::resource_publication_revision;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Every production expression that reads a raw `.ceiling` field, a raw
/// `$.ceiling` JSON path or the raw `Resource::catalog()` projection in the
/// three crates that handle resources, counted per file. Each entry says why
/// the value is not a resource's monthly maximum, or cannot differ from it.
///
/// A new raw read fails `raw_ceiling_readers_are_audited`. Read the maximum
/// through `resource_ceiling::effective_ceiling` (or `catalog_entry`), or add
/// the file here with the reason a raw read is right.
const RAW_READS: &[(&str, usize, &str)] = &[
    ("hagency/src/daemon/runtime.rs", 1, "dashboard projection reads resource.budget.pool.ceiling from the effective native resource budget"),
    ("hagency/src/console/native/owner_grant/types.rs", 1, "read-only reason derived from the effective budget snapshot"),
    (
        "hagency-core/src/allocation.rs",
        2,
        "allocation::Preset input; budget_with_grant fills it from effective_ceiling",
    ),
    (
        "hagency-core/src/project.rs",
        2,
        "qualifies() tests only that a maximum exists, which an amendment never changes; \
         catalog() is wrapped by resource_ceiling::catalog_entry",
    ),
    (
        "hagency-core/src/qualification.rs",
        1,
        "resources_for_role tests only that a maximum exists",
    ),
    (
        "hagency-store/src/domain/resource_ceiling.rs",
        3,
        "the accessor: the definition fallback and the effective catalogue projection",
    ),
    (
        "hagency-store/src/management/resource_edit.rs",
        1,
        "validates the edit or amendment request's CeilingChange input",
    ),
    (
        "hagency-store/src/domain/owner_resources/adoption/management/resource_ceiling.rs",
        1,
        "the amendment writer's CeilingChange input",
    ),
    (
        "hagency-store/src/domain/accounts.rs",
        1,
        "builds a new definition from an enrollment command",
    ),
    (
        "hagency-store/src/domain/resource_configuration.rs",
        6,
        "legacy configuration writer: its CeilingChange input and the definition it writes",
    ),
    (
        "hagency-store/src/domain/owner_resources/adoption/management/resource_edit.rs",
        3,
        "in-place edit writer: its CeilingChange input and the definition it writes; \
         refused once a resource is amended",
    ),
    (
        "hagency-store/src/domain/owner_resources/adoption/management/resource_create.rs",
        4,
        "new-definition writer: its CeilingChange input and the definition it writes; \
         Preserve copies effective_ceiling",
    ),
    (
        "hagency-store/src/domain/owner_resources/capacity.rs",
        1,
        "budget.pool.ceiling of a budget computed from effective_ceiling",
    ),
    (
        "hagency-store/src/domain/owner_resources/capacity/managed_agents/custody/shared_execution/attempt_work.rs",
        1,
        "budget.pool.ceiling of a budget computed from effective_ceiling",
    ),
    (
        "hagency-store/src/domain/owner_resources/capacity/managed_agents/custody/budget.rs",
        5,
        "an Agent budget head's own allocation ceiling, not the resource maximum",
    ),
    (
        "hagency-store/src/domain/owner_resources/capacity/managed_agents/custody/budget/journal.rs",
        2,
        "an Agent budget head's own allocation ceiling, not the resource maximum",
    ),
    (
        "hagency-store/src/domain/owner_resources/capacity/managed_agents/custody/budget/writer.rs",
        1,
        "an Agent budget head's own allocation ceiling, not the resource maximum",
    ),
    (
        "hagency/src/console/authority.rs",
        2,
        "console configure commands' CeilingChange input",
    ),
    (
        "hagency/src/console/native/owner_grant.rs",
        1,
        "budget.pool.ceiling of a budget computed from effective_ceiling",
    ),
    (
        "hagency/src/console/native/owner_grant/enrollment.rs",
        1,
        "budget.pool.ceiling of a budget computed from effective_ceiling",
    ),
    (
        "hagency/src/console/native/resource_edit.rs",
        2,
        "the request's CeilingChange input, and ResourceDetail::ceiling, \
         which the detail already fills from effective_ceiling",
    ),
];

/// Production text: full-line comments and inline `#[cfg(test)] mod … { … }`
/// bodies removed. Braces are counted outside string and char literals.
fn production(source: &str) -> String {
    let mut out = String::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        let attribute = trimmed.trim_end();
        if attribute == "#[cfg(test)]" || attribute.starts_with("#[cfg(all(test") {
            while lines
                .peek()
                .is_some_and(|next| next.trim_start().starts_with("#["))
            {
                lines.next();
            }
            let body = lines.peek().is_some_and(|next| {
                let next = next.trim();
                next.ends_with('{')
                    && (next.starts_with("mod ")
                        || next.starts_with("pub mod ")
                        || next.starts_with("pub(crate) mod ")
                        || next.starts_with("pub(super) mod "))
            });
            if body {
                let mut depth = 0i64;
                for inner in lines.by_ref() {
                    depth += braces(inner);
                    if depth <= 0 {
                        break;
                    }
                }
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn braces(line: &str) -> i64 {
    let mut depth = 0;
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' if quoted => {
                chars.next();
            }
            '"' => quoted = !quoted,
            '\'' if !quoted => {
                // A char literal such as '{' or '\'' never counts.
                let mut ahead = chars.clone();
                let first = ahead.next();
                let close = if first == Some('\\') {
                    ahead.next();
                    ahead.next()
                } else {
                    ahead.next()
                };
                if close == Some('\'') {
                    chars = ahead;
                }
            }
            '/' if !quoted && chars.peek() == Some(&'/') => break,
            '{' if !quoted => depth += 1,
            '}' if !quoted => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// Raw reads in production text: `.ceiling` not followed by an identifier
/// character (so `.ceiling_tokens` is a different, derived field), which also
/// covers SQL `$.ceiling` paths, plus raw catalogue projections.
fn raw_reads(production: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in production.lines() {
        let mut rest = line;
        while let Some(at) = rest.find(".ceiling") {
            let after = &rest[at + ".ceiling".len()..];
            if !after
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                found.push(line.trim().to_owned());
            }
            rest = after;
        }
        for token in [".catalog()", "[\"ceiling\"]", "get(\"ceiling\")"] {
            found.extend(std::iter::repeat_n(
                line.trim().to_owned(),
                line.matches(token).count(),
            ));
        }
    }
    found
}

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable source directory") {
        let path = entry.expect("readable source entry").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if name != "tests" {
                rust_files(&path, files);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            files.push(path);
        }
    }
}

#[test]
fn raw_ceiling_readers_are_audited() {
    let native = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the native workspace");
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for root in ["hagency-store/src", "hagency-core/src", "hagency/src"] {
        let mut files = Vec::new();
        rust_files(&native.join(root), &mut files);
        assert!(files.len() > 10, "{root} must be scanned, not skipped");
        for file in files {
            let source = std::fs::read_to_string(&file).expect("UTF-8 Rust source");
            let reads = raw_reads(&production(&source));
            if !reads.is_empty() {
                let relative = file
                    .strip_prefix(native)
                    .expect("inside the workspace")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.insert(relative, reads);
            }
        }
    }
    let allowed: BTreeMap<String, usize> = RAW_READS
        .iter()
        .map(|(file, count, reason)| {
            assert!(!reason.is_empty());
            ((*file).to_owned(), *count)
        })
        .collect();
    let counted: BTreeMap<String, usize> = found
        .iter()
        .map(|(file, reads)| (file.clone(), reads.len()))
        .collect();
    assert_eq!(
        counted, allowed,
        "a raw resource ceiling read changed; read the monthly maximum through \
         resource_ceiling::effective_ceiling or document the read in RAW_READS:\n{found:#?}"
    );
}

#[test]
fn the_audit_sees_reads_and_skips_tests_and_comments() {
    let source = "fn a(r: &Resource) -> bool { r.ceiling.is_some() }\n\
                  // r.ceiling in a comment\n\
                  fn b(r: &Report) -> u64 { r.ceiling_tokens.unwrap() }\n\
                  const Q: &str = \"SELECT json_extract(config,'$.ceiling.tokens')\";\n\
                  fn c(r: &Resource) { let _ = r.catalog(); let _ = v[\"ceiling\"]; }\n\
                  #[cfg(test)]\n\
                  mod tests {\n    fn t(r: &Resource) { let _ = (r.ceiling, '{'); }\n}\n\
                  fn d(r: &Resource) { let _ = r.ceiling; }\n";
    assert_eq!(raw_reads(&production(source)).len(), 5);
}

fn amended_fixture(ceiling: serde_json::Value) -> (tempfile::TempDir, DomainRepository, Resource) {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    let resource: Resource = serde_json::from_value(json!({
        "presetId":"amended_pool","seatId":"amended_seat","framework":"codex",
        "model":"gpt-5.6-sol","reasoning":"medium","published":true,"ceiling":ceiling
    }))
    .unwrap();
    db.put_resource(&resource).unwrap();
    (root, db, resource)
}
fn amend(
    db: &DomainRepository,
    resource: &Resource,
    sequence: u64,
    previous: u64,
    tokens: u64,
) -> rusqlite::Result<usize> {
    db.db.execute(
        "INSERT INTO owner_resource_ceiling_amendments(resource_id,sequence,profile,command_id,base_revision,previous_tokens,tokens,period,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,'monthly',1)",
        params![
            resource.id(),
            sequence,
            "a".repeat(64),
            format!("amend_{sequence}_{tokens}"),
            resource_publication_revision(resource).unwrap(),
            previous,
            tokens
        ],
    )
}

#[test]
fn the_latest_amendment_is_the_maximum_every_capacity_reader_uses() {
    let (_root, mut db, resource) = amended_fixture(json!({"tokens":1000,"period":"monthly"}));
    let id = resource.id();
    let revision = resource_publication_revision(&resource).unwrap();
    let stored = |db: &DomainRepository| {
        db.db
            .query_row("SELECT config FROM resources WHERE id=?1", [&id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
    };
    let definition = stored(&db);
    let now = graphs::now_ms().unwrap();
    assert!(matches!(
        check_grant(&db.db, &resource, "amended_agent", 1500, now),
        Err(Error::OverCommit { .. })
    ));
    amend(&db, &resource, 1, 1000, 2500).unwrap();
    // Budget, ceiling report and grant admission all read the amendment.
    let budget = budget_at(&db.db, &resource, None, false, now).unwrap();
    assert_eq!(budget.pool.ceiling.map(u64::from), Some(2500));
    assert_eq!(budget.remaining_tokens.map(u64::from), Some(2500));
    let report = usage::ceiling_report(&db.db, &id, now).unwrap();
    assert_eq!(report.ceiling_tokens, Some(2500));
    assert_eq!(report.period, UsagePeriodKind::Monthly);
    check_grant(&db.db, &resource, "amended_agent", 2500, now).unwrap();
    assert!(matches!(
        check_grant(&db.db, &resource, "amended_agent", 2501, now),
        Err(Error::OverCommit { .. })
    ));
    let summary = db.management_resource(&id, now).unwrap();
    assert_eq!(summary.revision, revision);
    assert_eq!(summary.budget.pool.ceiling.map(u64::from), Some(2500));
    assert_eq!(summary.usage.ceiling_tokens, Some(2500));
    let detail = db.management_resource_detail(&id, now).unwrap();
    assert_eq!(detail.resource.revision, revision);
    assert_eq!(
        detail.ceiling.and_then(|c| c.tokens).map(u64::from),
        Some(2500)
    );
    assert_eq!(detail.ceiling_amendment.sequence, 1);
    let entry = catalog_entry(&db.db, &resource).unwrap();
    assert_eq!(
        entry.ceiling.as_ref().and_then(|c| c.tokens).map(u64::from),
        Some(2500)
    );
    let configured = db.resource_configurations("", 10).unwrap();
    assert_eq!(
        configured[0]
            .effective_ceiling
            .as_ref()
            .and_then(|c| c.tokens)
            .map(u64::from),
        Some(2500)
    );
    assert_eq!(
        configured[0]
            .config
            .ceiling
            .as_ref()
            .and_then(|c| c.tokens)
            .map(u64::from),
        Some(1000),
        "the configuration readback is still the definition"
    );
    // Lowering takes effect the same way; the latest amendment always wins.
    amend(&db, &resource, 2, 2500, 800).unwrap();
    check_grant(&db.db, &resource, "amended_agent", 800, now).unwrap();
    assert!(matches!(
        check_grant(&db.db, &resource, "amended_agent", 801, now),
        Err(Error::OverCommit { .. })
    ));
    assert_eq!(
        db.resource_headroom(&id, now)
            .unwrap()
            .0
            .pool
            .ceiling
            .map(u64::from),
        Some(800)
    );
    // The definition, and so every pinned revision, never changed.
    assert_eq!(stored(&db), definition);
    assert_eq!(db.management_resource(&id, now).unwrap().revision, revision);
    // Publication changes only catalogue visibility, never the maximum.
    db.edit_resource(&resource, Some(false)).unwrap();
    assert_eq!(
        effective_ceiling_tokens(&db.db, &resource).unwrap(),
        Some(800)
    );
}

#[test]
fn amendment_records_continue_the_chain_and_are_immutable_and_retained() {
    let (_root, mut db, resource) = amended_fixture(json!({"tokens":1000,"period":"monthly"}));
    // A first record must start from the definition's own maximum, without gaps.
    assert!(amend(&db, &resource, 2, 1000, 1500).is_err());
    assert!(amend(&db, &resource, 1, 999, 1500).is_err());
    assert!(amend(&db, &resource, 1, 1000, 1000).is_err(), "unchanged");
    assert!(amend(&db, &resource, 1, 1000, 0).is_err(), "cleared");
    amend(&db, &resource, 1, 1000, 1500).unwrap();
    assert!(amend(&db, &resource, 2, 1000, 1200).is_err(), "fork");
    amend(&db, &resource, 2, 1500, 1200).unwrap();
    assert_eq!(
        latest_amendment(&db.db, &resource.id()).unwrap(),
        Some((2, 1200))
    );
    for sql in [
        "UPDATE owner_resource_ceiling_amendments SET tokens=9000",
        "UPDATE owner_resource_ceiling_amendments SET base_revision=printf('%064d',0)",
        "DELETE FROM owner_resource_ceiling_amendments",
        "DELETE FROM resources",
    ] {
        assert!(db.db.execute(sql, []).is_err(), "{sql}");
    }
    // The amended definition keeps the maximum the chain started from.
    let mut rewritten = resource.clone();
    rewritten.ceiling = Some(monthly(5000).unwrap());
    assert!(db.edit_resource(&rewritten, Some(true)).is_err());
    rewritten.ceiling = None;
    assert!(db.edit_resource(&rewritten, Some(true)).is_err());
    assert_eq!(
        effective_ceiling_tokens(&db.db, &resource).unwrap(),
        Some(1200)
    );
    assert_eq!(
        read_resource(&db.db, &resource.id())
            .unwrap()
            .ceiling
            .and_then(|c| c.tokens)
            .map(u64::from),
        Some(1000)
    );
    // A resource without a monthly maximum has nothing to amend.
    let (_daily_root, daily_db, daily) = amended_fixture(json!({"tokens":1000,"period":"daily"}));
    assert!(amend(&daily_db, &daily, 1, 1000, 1500).is_err());
    let (_none_root, none_db, none) = amended_fixture(serde_json::Value::Null);
    assert!(amend(&none_db, &none, 1, 0, 1500).is_err());
    assert_eq!(
        effective_ceiling(&none_db.db, &none).unwrap().map(|_| ()),
        None
    );
}
