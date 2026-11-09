//! Read-only inventory and historical evidence for retiring old allocations.
//! Historical imports can never authorize starting work or increasing capacity.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MigratedAuthority {
    legacy_adoption: String,
    source_digest: String,
    request: contract::AgentRequest,
    original_decisions: Vec<Value>,
}

pub(super) fn approved_request(raw: &str, allow_historical_retirement: bool) -> Result<contract::AgentRequest, Error> {
    let value: Value = serde_json::from_str(raw)?;
    if value.get("legacyAdoption").is_some() {
        if !allow_historical_retirement { return Err(Error::LocalAuthority); }
        Ok(serde_json::from_value::<MigratedAuthority>(value)?.request)
    } else {
        Ok(serde_json::from_value::<AgentApproval>(value)?.request)
    }
}

fn inventory(db: &Connection) -> Result<Value, Error> {
    let mut tables = Vec::new();
    let mut schema =
        db.prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")?;
    for table in schema.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })? {
        let (name, sql) = table?;
        if !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(Error::Schema);
        }
        let mut query = db.prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))?;
        let columns = query.column_count();
        let mut rows = query.query([])?;
        let mut hash = Sha256::new();
        let mut count = 0u64;
        while let Some(row) = rows.next()? {
            count = count.checked_add(1).ok_or(Error::Capacity)?;
            for column in 0..columns {
                use rusqlite::types::ValueRef;
                let (kind, bytes) = match row.get_ref(column)? {
                    ValueRef::Null => (0, Vec::new()),
                    ValueRef::Integer(value) => (1, value.to_le_bytes().to_vec()),
                    ValueRef::Real(value) => (2, value.to_bits().to_le_bytes().to_vec()),
                    ValueRef::Text(value) => (3, value.to_vec()),
                    ValueRef::Blob(value) => (4, value.to_vec()),
                };
                hash.update([kind]);
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
        }
        tables.push(json!({"table":name,"rows":count,"columns":columns,"schemaDigest":canonical::digest(&json!(sql))?,"digest":format!("{:x}",hash.finalize())}));
    }
    Ok(
        json!({"version":1,"schemaVersion":DOMAIN_SCHEMA_VERSION,"sourceDigest":canonical::digest(&json!(tables))?,"tables":tables}),
    )
}

impl DomainRepository {
    /// Read-only audit. The domain owner lock excludes an active runtime.
    /// Only counts/digests leave the store, never resource secrets or contexts.
    pub fn coordinator_audit_inventory(&self) -> Result<Value, Error> {
        inventory(&self.db)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn historical_authority_is_readable_only_for_retirement_not_new_spending() {
        let value = json!({"legacyAdoption":"old_import","sourceDigest":"a".repeat(64),
            "originalDecisions":[],"request":{"id":"request","revision":1,
            "serverEngagementId":format!("hf_{}","a".repeat(32)),"projectId":"project",
            "projectRevision":1,"resourceAllocationId":"grant","projectOwner":"@owner:example.test",
            "requester":"@owner:example.test","definitionDigest":"b".repeat(64),"requestedTokens":10}});
        let raw = value.to_string();
        assert!(approved_request(&raw, false).is_err());
        assert_eq!(approved_request(&raw, true).unwrap().id.as_str(), "request");
        assert_eq!(serde_json::from_str::<Value>(&raw).unwrap(), value);
    }
}
