//! Descriptive empty-device inventory, never initialization or source authority.
//! Native custody must prove absence of a personal file and retain exact source
//! identities, locks and SDK scope before a later initialization can use it.
use super::*;

/// Constructed only from read-only source connections, not caller JSON. A later
/// native writer must recapture it under source custody; a digest is not consent.
pub struct EmptyDeviceInventory(pub(super) Descriptor);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Descriptor {
    pub(super) version: u8,
    pub(super) source_schema: i32,
    pub(super) profile: String,
    pub(super) owner: String,
    pub(super) original_empty_seat: Option<Seat>,
    pub(super) personal_present: bool,
    pub(super) personal_digest: Option<String>,
    pub(super) custody_digest: String,
    pub(super) completed_private_records: u32,
}

impl EmptyDeviceInventory {
    pub fn digest(&self) -> Result<String, Error> {
        Ok(canonical::digest(&serde_json::to_value(&self.0)?)?)
    }
    pub fn personal_present(&self) -> bool {
        self.0.personal_present
    }
    pub fn completed_private_records(&self) -> u32 {
        self.0.completed_private_records
    }
    /// `None` describes a missing personal source. Only the native file-custody
    /// layer can establish that absence; this function cannot authorize it.
    pub fn inspect(
        domain: &mut Connection,
        mut personal: Option<&mut Connection>,
        profile: &str,
        owner: &str,
    ) -> Result<Self, Error> {
        hash(profile)?;
        palpo_hagency_contract::MatrixUserId::try_from(owner.to_owned())
            .map_err(|_| Error::LocalAuthority)?;
        if !domain.is_readonly("main")?
            || !domain.is_autocommit()
            || personal
                .as_ref()
                .is_some_and(|p| !p.is_readonly("main").unwrap_or(false) || !p.is_autocommit())
        {
            return Err(Error::LocalAuthority);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        domain.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        if let Some(p) = personal.as_mut() {
            p.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
        }
        let result = collect(
            domain,
            personal.as_deref_mut(),
            profile,
            owner,
            deadline,
            (false, false),
        );
        domain.progress_handler(0, None::<fn() -> bool>)?;
        if let Some(p) = personal {
            p.progress_handler(0, None::<fn() -> bool>)?;
        }
        if Instant::now() >= deadline {
            return Err(Error::Busy);
        }
        result.map(Self)
    }
}

pub(super) fn collect(
    domain: &mut Connection,
    personal: Option<&mut Connection>,
    profile: &str,
    owner: &str,
    deadline: Instant,
    archived: (bool, bool),
) -> Result<Descriptor, Error> {
    let d = domain.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let version: i32 = d.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if !matches!(version, 77..=111)
        || d.pragma_query_value(None, "application_id", |r| r.get::<_, i32>(0))?
            != if archived.0 { 0x48415231 } else { 0x48414732 }
    {
        return Err(Error::Schema);
    }
    schema::validate_domain(&d, version, deadline, archived.0)?;
    if d.query_row(
        "SELECT CASE WHEN length(profile_digest)=64 THEN profile_digest END FROM management_identity WHERE singleton=1",
        [], |r| r.get::<_, String>(0),
    )? != profile {
        return Err(Error::LocalAuthority);
    }
    for table in inventory::tables(&d)? {
        let n = inventory::count(&d, &table)?;
        match table.as_str() {
            "management_identity" if n == 1 => {}
            "management_archive_identity" if archived.0 && n == 1 => {}
            "seats" if n <= 1 => {}
            // Only closed, validated terminal custody is accepted below. It is
            // retained in its original journal, never erased by setup.
            "management_commands" | "management_intents" => {}
            _ if n == 0 => {}
            _ => return Err(Error::Quarantined),
        }
    }
    let seat = d
        .query_row(
            "SELECT CASE WHEN length(CAST(config AS BLOB))<=16384 THEN config END FROM seats",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(|raw| serde_json::from_str::<Seat>(&raw))
        .transpose()?;
    if let Some(seat) = &seat {
        seat.validate()?;
        if seat.declaration.is_some() {
            return Err(Error::Quarantined);
        }
        let id: String = d.query_row("SELECT id FROM seats", [], |r| r.get(0))?;
        if id != seat.id {
            return Err(Error::Quarantined);
        }
    }
    let personal_present = personal.is_some();
    let mut personal_digest = None;
    if let Some(personal) = personal {
        let p = personal.transaction_with_behavior(TransactionBehavior::Deferred)?;
        if p.pragma_query_value(None, "application_id", |r| r.get::<_, i32>(0))?
            != if archived.1 { 0x48415231 } else { 0x48414c32 }
            || p.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))? != 1
        {
            return Err(Error::Schema);
        }
        schema::validate_personal(&p, deadline, archived.1)?;
        if p.query_row("SELECT CASE WHEN length(digest)=64 THEN digest END FROM profile_identity WHERE singleton=1", [], |r| r.get::<_, String>(0))? != profile
            || p.query_row("SELECT CASE WHEN length(CAST(owner AS BLOB))<=255 THEN owner END FROM identity", [], |r| r.get::<_, String>(0))? != owner
        { return Err(Error::LocalAuthority); }
        for table in inventory::tables(&p)? {
            let n = inventory::count(&p, &table)?;
            match table.as_str() {
                "identity" | "profile_identity" | "inbox_limits" if n == 1 => {}
                "personal_archive_identity" if archived.1 && n == 1 => {}
                _ if n == 0 => {}
                _ => return Err(Error::Quarantined),
            }
        }
        let limits: (i64, i64) = p.query_row(
            "SELECT max_records,max_bytes FROM inbox_limits WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        personal_digest = Some(canonical::digest(
            &json!({"profile":profile,"owner":owner,"inboxLimits":limits}),
        )?);
    }
    let (custody_digest, completed_private_records) = custody::collect(&d, &[], true)?;
    Ok(Descriptor {
        version: 1,
        source_schema: version,
        profile: profile.into(),
        owner: owner.into(),
        original_empty_seat: seat,
        personal_present,
        personal_digest,
        custody_digest,
        completed_private_records,
    })
}

#[cfg(test)]
mod tests;
