use super::*;
const MAX_ROWS: u32 = 10_000;
/// Read from exact already-owned read-only connections. This is an inventory,
/// not evidence of physical locks, complete external discovery, or adoption.
pub struct AdoptionInventory(
    pub(super) Inventory,
    pub(super) Option<task_transfer::History>,
    pub(super) Option<alert_transfer::History>,
);
impl AdoptionInventory {
    /// Native descriptive recovery only. This comparison does not create a
    /// plan, provider proof, physical source fence or import authority.
    pub fn matches_inventory_digest(&self, expected: &str) -> Result<bool, Error> {
        Ok(canonical::digest(&serde_json::to_value(&self.0)?)? == expected)
    }
    pub fn reviewed_resources(&self) -> Result<Vec<(String, String)>, Error> {
        self.0.resources.iter().map(|r| Ok((r.id(), crate::resource_publication_revision(r)?))).collect()
    }

    pub fn matches_review(&self, expected: &[(String, String)]) -> Result<bool, Error> {
        let actual = self
            .0
            .resources
            .iter()
            .map(|r| Ok((r.id(), crate::resource_publication_revision(r)?)))
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(actual == expected)
    }
    pub fn inspect(
        domain: &mut Connection,
        personal: &mut Connection,
        profile: &str,
        owner: &str,
    ) -> Result<Self, Error> {
        hash(profile)?;
        palpo_hagency_contract::MatrixUserId::try_from(owner.to_owned())
            .map_err(|_| Error::LocalAuthority)?;
        if !domain.is_readonly("main")?
            || !personal.is_readonly("main")?
            || !domain.is_autocommit()
            || !personal.is_autocommit()
        {
            return Err(Error::LocalAuthority);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        domain.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        personal.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        let result = collect(domain, personal, profile, owner, deadline, (false, false));
        domain.progress_handler(0, None::<fn() -> bool>)?;
        personal.progress_handler(0, None::<fn() -> bool>)?;
        if Instant::now() >= deadline {
            return Err(Error::Busy);
        }
        result.map(|v| Self(v, None, None))
    }
    /// Explicit typed v2 capture on original already-owned connections only.
    /// The caller must retain source custody through plan persistence/fencing.
    pub fn inspect_with_operator_tasks(
        domain: &mut Connection,
        personal: &mut Connection,
        profile: &str,
        owner: &str,
    ) -> Result<Self, Error> {
        hash(profile)?;
        palpo_hagency_contract::MatrixUserId::try_from(owner.to_owned())
            .map_err(|_| Error::LocalAuthority)?;
        if !domain.is_readonly("main")?
            || !personal.is_readonly("main")?
            || !domain.is_autocommit()
            || !personal.is_autocommit()
        {
            return Err(Error::LocalAuthority);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        domain.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        personal.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        let result = collect_inner(
            domain,
            personal,
            profile,
            owner,
            deadline,
            (false, false),
            HistoryMode::Capture,
        );
        domain.progress_handler(0, None::<fn() -> bool>)?;
        personal.progress_handler(0, None::<fn() -> bool>)?;
        if Instant::now() >= deadline {
            return Err(Error::Busy);
        }
        result.map(|(i, h, a)| Self(i, h, a))
    }
    /// Explicit v3 capture includes original resource alert history and notes.
    pub fn inspect_with_operator_history(
        domain: &mut Connection,
        personal: &mut Connection,
        profile: &str,
        owner: &str,
    ) -> Result<Self, Error> {
        hash(profile)?;
        palpo_hagency_contract::MatrixUserId::try_from(owner.to_owned())
            .map_err(|_| Error::LocalAuthority)?;
        if !domain.is_readonly("main")?
            || !personal.is_readonly("main")?
            || !domain.is_autocommit()
            || !personal.is_autocommit()
        {
            return Err(Error::LocalAuthority);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        domain.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        personal.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        let result = collect_inner(
            domain,
            personal,
            profile,
            owner,
            deadline,
            (false, false),
            HistoryMode::CaptureAlerts,
        );
        domain.progress_handler(0, None::<fn() -> bool>)?;
        personal.progress_handler(0, None::<fn() -> bool>)?;
        if Instant::now() >= deadline {
            return Err(Error::Busy);
        }
        result.map(|(i, h, a)| Self(i, h, a))
    }
}
pub(super) fn tables(db: &Connection) -> Result<Vec<String>, Error> {
    let names=db.prepare("SELECT CASE WHEN length(CAST(name AS BLOB))<=128 THEN name END FROM pragma_table_list WHERE schema='main' AND name NOT LIKE 'sqlite_%' AND type NOT IN ('view') ORDER BY name LIMIT 257")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    if names.len() > 256 {
        return Err(Error::Capacity);
    }
    for name in &names {
        if name.len() > 128 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(Error::Schema);
        }
        let ordinary: bool = db.query_row(
            "SELECT type='table' FROM pragma_table_list WHERE schema='main' AND name=?1",
            [name],
            |r| r.get(0),
        )?;
        if !ordinary {
            return Err(Error::Schema);
        }
    }
    Ok(names)
}
pub(super) fn count(db: &Connection, table: &str) -> Result<u32, Error> {
    let count = db.query_row(
        &format!(
            "SELECT count(*) FROM (SELECT 1 FROM \"{table}\" LIMIT {})",
            MAX_ROWS + 1
        ),
        [],
        |r| r.get::<_, u32>(0),
    )?;
    if count > MAX_ROWS {
        return Err(Error::Capacity);
    }
    Ok(count)
}
pub(super) fn collect(
    domain: &mut Connection,
    personal: &mut Connection,
    profile: &str,
    owner: &str,
    deadline: Instant,
    archived: (bool, bool),
) -> Result<Inventory, Error> {
    collect_inner(
        domain,
        personal,
        profile,
        owner,
        deadline,
        archived,
        HistoryMode::None,
    )
    .map(|v| v.0)
}
#[derive(Clone, Copy)]
enum HistoryMode<'a> {
    None,
    Capture,
    CaptureAlerts,
    ArchivedAlerts(&'a task_transfer::Summary, &'a alert_transfer::Summary),
    Archived(&'a task_transfer::Summary),
}
/// A v2 archive's original payload is already canonical. Do not rescan archived
/// task data; only exact permanent provenance and table counts are checked.
pub(super) fn collect_for_plan(
    domain: &mut Connection,
    personal: &mut Connection,
    plan: &Plan,
    deadline: Instant,
    archived: (bool, bool),
) -> Result<Inventory, Error> {
    alert_transfer::verify(plan)?;
    let mode = match (plan.version, plan.inventory.task_history.as_ref()) {
        (1, None) => HistoryMode::None,
        (2, Some(summary)) => {
            summary.validate()?;
            if archived.0 {
                HistoryMode::Archived(summary)
            } else {
                HistoryMode::Capture
            }
        }
        (3, Some(summary)) => {
            summary.validate()?;
            let alerts = plan.inventory.alert_history.as_ref().ok_or(Error::Quarantined)?;
            alerts.validate()?;
            if archived.0 { HistoryMode::ArchivedAlerts(summary, alerts) } else { HistoryMode::CaptureAlerts }
        }
        _ => return Err(Error::Quarantined),
    };
    collect_inner(
        domain,
        personal,
        &plan.inventory.profile,
        &plan.inventory.owner,
        deadline,
        archived,
        mode,
    )
    .map(|v| v.0)
}
type CapturedInventory = (Inventory, Option<task_transfer::History>, Option<alert_transfer::History>);
#[allow(clippy::too_many_arguments)]
fn collect_inner(
    domain: &mut Connection,
    personal: &mut Connection,
    profile: &str,
    owner: &str,
    deadline: Instant,
    archived: (bool, bool),
    history: HistoryMode<'_>,
) -> Result<CapturedInventory, Error> {
    let d = domain.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let p = personal.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let schema: i32 = d.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if !matches!(schema, 77..=111)
        || d.pragma_query_value(None, "application_id", |r| r.get::<_, i32>(0))?
            != if archived.0 { 0x48415231 } else { 0x48414732 }
        || p.pragma_query_value(None, "application_id", |r| r.get::<_, i64>(0))?
            != if archived.1 { 0x48415231 } else { 0x48414c32 }
        || p.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))? != 1
    {
        return Err(Error::Schema);
    }
    // Verify every table/view/trigger/index before any payload query. An
    // omitted history table or a view impersonating identity/resources must
    // never turn unknown custody into an apparently empty source.
    schema::validate(&d, &p, schema, deadline, archived)?;
    if d.query_row(
        "SELECT CASE WHEN length(profile_digest)=64 THEN profile_digest END FROM management_identity WHERE singleton=1",
        [],
        |r| r.get::<_, String>(0),
    )? != profile
        || p.query_row(
            "SELECT CASE WHEN length(digest)=64 THEN digest END FROM profile_identity WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )? != profile
        || p.query_row("SELECT CASE WHEN length(CAST(owner AS BLOB))<=255 THEN owner END FROM identity", [], |r| r.get::<_, String>(0))? != owner
    {
        return Err(Error::LocalAuthority);
    }
    for table in tables(&p)? {
        let n = count(&p, &table)?;
        match table.as_str() {
            "identity" | "profile_identity" | "inbox_limits" if n == 1 => {}
            "personal_archive_identity" if archived.1 && n == 1 => {}
            _ if n == 0 => {}
            _ => return Err(Error::Quarantined),
        }
    }
    for table in tables(&d)? {
        let n = count(&d, &table)?;
        match table.as_str() {
            "management_identity" if n == 1 => {}
            "management_archive_identity" if archived.0 && n == 1 => {}
            "seats" if n == 1 => {}
            "resources" if (1..=16).contains(&n) => {}
            "management_commands" | "management_intents" => {}
            "ceiling_alerts" | "ceiling_alert_notes"
                if matches!(history, HistoryMode::CaptureAlerts | HistoryMode::ArchivedAlerts(..)) => {}
            "operator_tasks" | "operator_task_comments"
                if !matches!(history, HistoryMode::None) => {}
            _ if n == 0 => {}
            _ => return Err(Error::Quarantined),
        }
    }
    let seat: Seat = serde_json::from_str(&d.query_row(
        "SELECT CASE WHEN length(CAST(config AS BLOB))<=16384 THEN config END FROM seats",
        [],
        |r| r.get::<_, String>(0),
    )?)?;
    seat.validate()?;
    let rows = d
        .prepare("SELECT CASE WHEN length(id)<=128 THEN id END,CASE WHEN length(CAST(config AS BLOB))<=16384 THEN config END FROM resources ORDER BY id LIMIT 17")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut resources = Vec::new();
    for (id, raw) in rows {
        if raw.len() > 16 * 1024 {
            return Err(Error::Capacity);
        }
        let r: Resource = serde_json::from_str(&raw)?;
        r.validate()?;
        if r.id() != id || r.seat_id != seat.id {
            return Err(Error::Quarantined);
        }
        resources.push(r);
    }
    if resources.is_empty() {
        return Err(Error::Unqualified);
    }
    let (task_history, captured) = match history {
        HistoryMode::None => (None, None),
        HistoryMode::Capture | HistoryMode::CaptureAlerts => {
            if archived.0 {
                return Err(Error::LocalAuthority);
            }
            let h = task_transfer::capture(&d, deadline)?;
            (Some(h.summary()?), Some(h))
        }
        HistoryMode::Archived(summary) | HistoryMode::ArchivedAlerts(summary, _) => {
            // No archived task payload is used as adoption or read authority.
            if count(&d, "operator_tasks")? != summary.tasks
                || count(&d, "operator_task_comments")? != summary.comments
            {
                return Err(Error::Quarantined);
            }
            (Some(summary.clone()), None)
        }
    };
    let (alert_history, alerts) = match history {
        HistoryMode::CaptureAlerts => {
            let h = alert_transfer::capture(&d, &resources, deadline)?;
            (Some(h.summary(&resources)?), Some(h))
        }
        HistoryMode::ArchivedAlerts(_, summary) => {
            if count(&d, "ceiling_alerts")? != summary.alerts || count(&d, "ceiling_alert_notes")? != summary.notes { return Err(Error::Quarantined); }
            (Some(summary.clone()), None)
        }
        _ => (None, None),
    };
    let (custody_digest, custody_rows) = custody::collect(&d, &resources, alert_history.is_some())?;
    let result = Inventory {
        task_history,
        alert_history,
        source_schema: schema,
        profile: profile.into(),
        owner: owner.into(),
        resources,
        seat,
        custody_digest,
        custody_rows,
        personal_digest: canonical::digest(
            &json!({"owner":owner,"profile":profile,"tables":tables(&p)?.into_iter().filter(|name|name!="personal_archive_identity").collect::<Vec<_>>(),"inboxLimits":p.query_row("SELECT max_records,max_bytes FROM inbox_limits WHERE singleton=1",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?)))?,"emptyRuntimeHistory":true}),
        )?,
    };
    p.commit()?;
    d.commit()?;
    Ok((result, captured, alerts))
}
