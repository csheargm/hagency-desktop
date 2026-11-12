//! Creation proof and irreversible first exposure belong to the installation,
//! not one profile's paginated remote catalog or disposable command journal.
use super::*;
use hagency_core::project::identifier;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum History {
    Unknown,
    NeverOffered,
    Offered,
}

pub(super) enum OfferKind {
    Contribution,
    CapacityGrant,
}
impl OfferKind {
    fn name(&self) -> &'static str {
        match self {
            Self::Contribution => "contribution",
            Self::CapacityGrant => "capacity_grant",
        }
    }
}

fn registry(db: &Connection, expected: &str) -> Result<(), Error> {
    hash(expected)?;
    let current: String = db.query_row(
        "SELECT id FROM owner_resource_identity WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if current != expected {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}

/// Only the branch which actually inserted a new resource calls this, after
/// recording its original enrollment receipt in the same transaction.
pub(super) fn created(
    db: &Connection,
    receipt: &OwnerResourceReceipt,
    digest: &str,
    profile: &str,
) -> Result<(), Error> {
    registry(db, &receipt.registry_id)?;
    hash(digest)?;
    hash(profile)?;
    let (saved_digest, saved): (String, String) = db.query_row(
        "SELECT digest,result FROM owner_resource_receipts WHERE command_id=?1",
        [&receipt.command_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if saved_digest != digest
        || serde_json::from_str::<Value>(&saved)? != serde_json::to_value(receipt)?
    {
        return Err(Error::Quarantined);
    }
    db.execute(
        "INSERT INTO owner_resource_provenance(resource_id,registry_id,creation_command_id,creation_digest,creation_profile) VALUES(?1,?2,?3,?4,?5)",
        params![receipt.resource_id,receipt.registry_id,receipt.command_id,digest,profile],
    )?;
    if history(db, &receipt.registry_id, &receipt.resource_id)? != History::NeverOffered {
        return Err(Error::Quarantined);
    }
    Ok(())
}

/// The caller retains current profile authority and an immediate transaction.
/// A repeated/later offer never overwrites the first exposure, even if its
/// request was refused, its response was lost, or the remote grant later ends.
pub(super) fn offered(
    db: &Connection,
    registry_id: &str,
    profile: &str,
    resource_id: &str,
    command_id: &str,
    expected_revision: &str,
    kind: OfferKind,
) -> Result<(), Error> {
    registry(db, registry_id)?;
    hash(profile)?;
    hash(expected_revision)?;
    identifier(command_id, 128)?;
    let current = read_resource(db, resource_id)?;
    if crate::resource_publication_revision(&current)? != expected_revision {
        return Err(Error::Conflict);
    }
    let previous: Option<String> = db
        .query_row(
            "SELECT registry_id FROM owner_resource_provenance WHERE resource_id=?1",
            [resource_id],
            |row| row.get(0),
        )
        .optional()?;
    if previous.as_deref().is_some_and(|id| id != registry_id) {
        return Err(Error::Quarantined);
    }
    if previous.is_none() {
        // An old/imported resource gets exposure history, never invented
        // creation proof. Missing original creation remains unknown forever.
        db.execute(
            "INSERT INTO owner_resource_provenance(resource_id,registry_id) VALUES(?1,?2)",
            params![resource_id, registry_id],
        )?;
    }
    db.execute(
        "UPDATE owner_resource_provenance SET first_offer_kind=?2,first_offer_profile=?3,first_offer_command_id=?4,first_offer_revision=?5,first_offer_at_ms=?6 WHERE resource_id=?1 AND first_offer_kind IS NULL",
        params![resource_id,kind.name(),profile,command_id,expected_revision,graphs::now_ms()?],
    )?;
    Ok(())
}

struct StoredHistory {
    registry: String,
    command: Option<String>,
    digest: Option<String>,
    offered: Option<String>,
}

pub(super) fn history(
    db: &Connection,
    registry_id: &str,
    resource_id: &str,
) -> Result<History, Error> {
    registry(db, registry_id)?;
    let row: Option<StoredHistory> = db.query_row(
        "SELECT registry_id,creation_command_id,creation_digest,first_offer_kind FROM owner_resource_provenance WHERE resource_id=?1",
        [resource_id], |row| Ok(StoredHistory {registry:row.get(0)?,command:row.get(1)?,digest:row.get(2)?,offered:row.get(3)?}),
    ).optional()?;
    let Some(row) = row else {
        return Ok(History::Unknown);
    };
    if row.registry != registry_id {
        return Err(Error::Quarantined);
    }
    if row.offered.is_some() {
        return Ok(History::Offered);
    }
    let (Some(command), Some(digest)) = (row.command, row.digest) else {
        return Ok(History::Unknown);
    };
    let saved: Option<(String, String)> = db
        .query_row(
            "SELECT digest,result FROM owner_resource_receipts WHERE command_id=?1",
            [&command],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((actual_digest, result)) = saved else {
        return Err(Error::Quarantined);
    };
    let receipt: OwnerResourceReceipt = serde_json::from_str(&result)?;
    if digest != actual_digest
        || receipt.command_id != command
        || receipt.registry_id != registry_id
        || receipt.resource_id != resource_id
        || !receipt.active
    {
        return Err(Error::Quarantined);
    }
    Ok(History::NeverOffered)
}

fn scope(registry: &OwnerResourceRegistry, profile: &ProfileAuthority) -> Result<(), Error> {
    profile.check()?;
    if registry.0.closed.load(Ordering::Acquire) {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}

impl OwnerResourceRegistry {
    pub(crate) async fn record_contribution_attempt(
        &self,
        profile: ProfileAuthority,
        resource_id: String,
        expected_revision: String,
        command_id: String,
    ) -> Result<(), Error> {
        scope(self, &profile)?;
        let registry = self.clone();
        self.0
            .store
            .call(8192, move |db| {
                scope(&registry, &profile)?;
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                scope(&registry, &profile)?;
                authorize_resource(&tx, &profile, &resource_id)?;
                offered(
                    &tx,
                    registry.id(),
                    &profile.digest,
                    &resource_id,
                    &command_id,
                    &expected_revision,
                    OfferKind::Contribution,
                )?;
                scope(&registry, &profile)?;
                tx.commit()?;
                scope(&registry, &profile).map_err(|_| Error::OutcomeUnknown)
            })
            .await
    }
}
