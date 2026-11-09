//! Native graph commands backed by one canonical graph/outbox transaction.
//!
//! This is a native domain adapter, not a mini-app service or a dispatch grant.
//! Assignee names remain recorded labels. Outbox entries wait for a separately
//! reviewed current recipient binding; persisting one never starts a provider.
use super::{Graph, GraphError, normalize_graph, iso_from_millis, transitions};
use hagency_core::InvalidInput;
use hagency_store::{
    Error,
    owner_resources::{
        OwnerResourceRegistry, ProfileAuthority,
        graph_journal::{GraphJournalCommand, GraphJournalReceipt},
    },
};
use serde::{Deserialize, Serialize, de};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorGraphNodeInput {
    pub assignee: String,
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub condition: Option<Value>,
}

/// Missing result/error members preserve the original value; null clears it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorGraphNodePatch {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "hagency_store::management::TaskField::is_unchanged"
    )]
    pub result: hagency_store::management::TaskField<Value>,
    #[serde(
        default,
        skip_serializing_if = "hagency_store::management::TaskField::is_unchanged"
    )]
    pub error: hagency_store::management::TaskField<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorGraphChange {
    Create {
        label: String,
        nodes: BTreeMap<String, OperatorGraphNodeInput>,
    },
    UpdateNode {
        patch: OperatorGraphNodePatch,
    },
    BindRecipient {
        #[serde(rename = "nodeId")]
        node_id: String,
        snapshot: super::recipients::OperatorGraphRecipientSnapshot,
    },
    Cancel,
}

/// A native-selected scope is distinct from a management project ID or an
/// archived source root. This adapter does not infer either association.
pub struct OperatorGraphJournal {
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
    scope_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorGraphSnapshot {
    pub revision: u64,
    pub graph: Value,
    /// The journal alone has neither recipient bindings nor runtime authority.
    pub runtime_delivery_authorized: bool,
}

fn exact(value: &str, max: usize) -> Result<(), Error> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().count() > max
        || value.contains('\0')
    {
        return Err(InvalidInput("invalid graph text").into());
    }
    Ok(())
}
pub(crate) fn exact_id(value: &str) -> Result<(), Error> {
    exact(value, 255)?;
    if value.chars().any(char::is_control) {
        return Err(InvalidInput("invalid graph identifier").into());
    }
    Ok(())
}
const MAX_DRAFT_JSON_BYTES: usize = 128 * 1024;
const MAX_DRAFT_JSON_ITEMS: usize = 10_000;

pub(crate) fn parse_draft_json(raw: &str) -> Result<Value, Error> {
    if raw.len() > MAX_DRAFT_JSON_BYTES {
        return Err(Error::Capacity);
    }
    serde_json::from_slice::<UniqueDraftValue>(raw.as_bytes())
        .map(|value| value.0)
        .map_err(|_| InvalidInput("invalid graph JSON").into())
}

/// A reviewed draft must have a single interpretation. Ordinary `Value`
/// deserialization silently replaces duplicate object keys, including nested
/// keys in arbitrary result/condition data. Keep this validation at the current
/// command boundary, with byte, collection and serde recursion limits.
struct UniqueDraftValue(Value);
impl<'de> Deserialize<'de> for UniqueDraftValue {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = UniqueDraftValue;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("unambiguous graph JSON")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueDraftValue(Value::Null))
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueDraftValue(Value::Bool(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueDraftValue(Value::Number(value.into())))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueDraftValue(Value::Number(value.into())))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueDraftValue(Value::Number(number)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueDraftValue(Value::String(value.into())))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(UniqueDraftValue(value)) = seq.next_element()? {
                    if values.len() >= MAX_DRAFT_JSON_ITEMS {
                        return Err(de::Error::custom("graph JSON item limit"));
                    }
                    values.push(value);
                }
                Ok(UniqueDraftValue(Value::Array(values)))
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, UniqueDraftValue(value))) = map.next_entry::<String, UniqueDraftValue>()? {
                    if values.len() >= MAX_DRAFT_JSON_ITEMS {
                        return Err(de::Error::custom("graph JSON item limit"));
                    }
                    if values.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate graph JSON key"));
                    }
                }
                Ok(UniqueDraftValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
fn domain_error(error: GraphError) -> Error {
    InvalidInput(error.code).into()
}
pub(crate) fn bounded_input(change: &OperatorGraphChange) -> Result<(), Error> {
    let max = if matches!(change, OperatorGraphChange::Create { .. }) {
        64 * 1024
    } else {
        128 * 1024
    };
    if serde_json::to_vec(change)?.len() > max {
        return Err(Error::Capacity);
    }
    match change {
        OperatorGraphChange::Create { label, nodes } => {
            exact(label, 4000)?;
            if nodes.is_empty() || nodes.len() > 256 {
                return Err(Error::Capacity);
            }
            for (id, node) in nodes {
                exact_id(id)?;
                exact(&node.assignee, 255)?;
                exact(&node.description, 4000)?;
                if node.depends_on.len() > 256 {
                    return Err(Error::Capacity);
                }
                for id in &node.depends_on {
                    exact_id(id)?;
                }
                // The retained normalizer validates cycles, references and conditions.
            }
        }
        OperatorGraphChange::UpdateNode { patch } => {
            exact_id(&patch.node_id)?;
            if matches!(&patch.error, hagency_store::management::TaskField::Set(Some(error)) if error.chars().count() > 4000)
            {
                return Err(InvalidInput("graph error exceeds 4000 characters").into());
            }
            if let Some(status) = &patch.status {
                exact(status, 32)?;
            }
        }
        OperatorGraphChange::Cancel => {}
        OperatorGraphChange::BindRecipient { node_id, snapshot } => {
            exact_id(node_id)?;
            snapshot.validate()?;
            if snapshot.node_id != *node_id { return Err(InvalidInput("recipient node mismatch").into()); }
        }
    }
    Ok(())
}

impl OperatorGraphChange {
    /// Validate a reviewed payload without creating graph or outbox state.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        bounded_input(self)?;
        match self {
            Self::Create { label, nodes } => {
                let nodes: serde_json::Map<String, Value> = nodes.iter().map(|(id, node)| {
                    (id.clone(), json!({"id":id,"assignee":node.assignee,"description":node.description,"depends_on":node.depends_on,"condition":node.condition}))
                }).collect();
                normalize_graph(&json!({"id":"review","owner":"review","label":label,"nodes":nodes,"createdAt":"1970-01-01T00:00:00.000Z","updatedAt":"1970-01-01T00:00:00.000Z"}), None).map_err(domain_error)?;
            }
            Self::UpdateNode { patch } => {
                if patch.status.is_none()
                    && patch.result.is_unchanged()
                    && patch.error.is_unchanged()
                {
                    return Err(InvalidInput("node patch requires status, result, or error").into());
                }
                if let Some(status) = &patch.status
                    && !super::node_statuses().contains(&status.as_str())
                {
                    return Err(InvalidInput("invalid node status").into());
                }
                if let hagency_store::management::TaskField::Set(result) = &patch.result {
                    super::normalize_result(result.as_ref()).map_err(domain_error)?;
                }
            }
            Self::Cancel => {}
            Self::BindRecipient { .. } => {}
        }
        Ok(())
    }
}

impl OperatorGraphJournal {
    /// Only the native authenticated host can construct ProfileAuthority. No
    /// caller-selected storage path or legacy-file writer is opened here.
    pub fn new(
        registry: OwnerResourceRegistry,
        profile: ProfileAuthority,
        scope_id: String,
    ) -> Result<Self, Error> {
        hagency_core::project::identifier(&scope_id, 128)?;
        Ok(Self {
            registry,
            profile,
            scope_id,
        })
    }

    pub async fn read(&self, graph_id: String) -> Result<Option<OperatorGraphSnapshot>, Error> {
        self.profile.check_current()?;
        let Some(record) = self
            .registry
            .operator_graph_record(&self.profile, &self.scope_id, &graph_id)
            .await?
        else {
            return Ok(None);
        };
        let graph: Graph = serde_json::from_slice(&record.document)?;
        if graph.id != graph_id || graph.owner != self.profile.account() {
            return Err(Error::Quarantined);
        }
        let graph = serde_json::to_value(graph)?;
        self.profile.check_current()?;
        Ok(Some(OperatorGraphSnapshot {
            revision: record.revision,
            graph,
            runtime_delivery_authorized: false,
        }))
    }

    /// Native IDs and exact reviewed revisions are retained by the caller's
    /// existing prepare/review/apply command journal. The same command returns
    /// its original compact receipt even after later changes and process restart.
    pub async fn apply(
        &self,
        command_id: String,
        graph_id: String,
        expected_revision: u64,
        change: OperatorGraphChange,
    ) -> Result<GraphJournalReceipt, Error> {
        change.validate()?;
        exact_id(&graph_id)?;
        if let OperatorGraphChange::BindRecipient { snapshot, .. } = &change
            && (snapshot.graph_id != graph_id || snapshot.revision != expected_revision || snapshot.account != self.profile.account()) {
            return Err(InvalidInput("recipient graph scope mismatch").into());
        }
        if matches!(&change, OperatorGraphChange::Create { .. }) != (expected_revision == 0) {
            return Err(InvalidInput("graph create requires revision zero").into());
        }
        // Results and conditions are arbitrary retained JSON, not integer-only signed DTOs.
        let input_digest = hagency_core::project::hash(&serde_json::to_vec(&change)?);
        let owner = self.profile.account().to_owned();
        let command = GraphJournalCommand {
            command_id,
            scope_id: self.scope_id.clone(),
            graph_id: graph_id.clone(),
            expected_revision,
            input_digest,
        };
        self.registry.transact_operator_graph(&self.profile, command, move |record, writer| {
            let timestamp_ms = u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| Error::LocalAuthority)?.as_millis()).map_err(|_| Error::Capacity)?;
            let at = iso_from_millis(timestamp_ms);
            let mut graph = match &change {
                OperatorGraphChange::Create { label, nodes } => {
                    let nodes: serde_json::Map<String, Value> = nodes.iter().map(|(id,node)| (id.clone(), json!({"id":id,"assignee":node.assignee,"description":node.description,"depends_on":node.depends_on,"condition":node.condition}))).collect();
                    normalize_graph(&json!({"id":graph_id,"owner":owner,"label":label,"nodes":nodes,"createdAt":at,"updatedAt":at}), None).map_err(domain_error)?
                }
                _ => {
                    let graph: Graph = serde_json::from_slice(&record.ok_or(Error::NotFound)?.document)?;
                    if graph.id != graph_id || graph.owner != owner { return Err(Error::Quarantined); }
                    graph
                }
            };
            match &change {
                OperatorGraphChange::Create { .. } => {}
                OperatorGraphChange::UpdateNode { patch } => {
                    let mut value = serde_json::to_value(patch)?;
                    value.as_object_mut().ok_or(Error::Quarantined)?.remove("nodeId");
                    graph = transitions::patch_node(&graph, &patch.node_id, &value, &at).map_err(domain_error)?.graph;
                }
                OperatorGraphChange::Cancel => {
                    graph = transitions::cancel(&graph, &at).graph;
                    writer.suppress_pending()?;
                }
                OperatorGraphChange::BindRecipient { node_id, snapshot } => {
                    let node = graph.nodes.get(node_id).ok_or(Error::NotFound)?;
                    if graph.status != "active" || !matches!(node.status.as_str(), "pending" | "dispatched") {
                        return Err(Error::Conflict);
                    }
                    writer.bind_recipient(node_id, serde_json::to_vec(snapshot)?)?;
                    // Recipient selection is metadata only. Do not advance nodes,
                    // rebuild a message or modify an existing held envelope.
                    return serde_json::to_vec(&graph).map_err(Error::from);
                }
            }
            // The shared engine calls this only for a ready node. enqueue returns
            // its original identity on retry and does not rebuild private tokens.
            let mut dispatch_error = None;
            graph = transitions::advance(&graph, &at, |g,n| {
                writer.enqueue(&n.id, |id| {
                    serde_json::to_vec(&transitions::dispatch_message(g,n,id,timestamp_ms,&super::view_token())).map_err(Error::from)
                }).map_err(|error| {
                    dispatch_error = Some(error);
                    super::graph_error("graph_dispatch_failed", "canonical graph outbox could not be committed")
                })
            }).map_err(|error| dispatch_error.unwrap_or_else(|| domain_error(error)))?.graph;
            serde_json::to_vec(&graph).map_err(Error::from)
        }).await
    }
    pub async fn original_receipt(&self, command_id:String, graph_id:String, expected_revision:u64, change:&OperatorGraphChange) -> Result<Option<GraphJournalReceipt>,Error> {
        change.validate()?;
        self.registry.operator_graph_command_receipt(&self.profile, GraphJournalCommand {
            command_id, scope_id:self.scope_id.clone(), graph_id, expected_revision,
            input_digest:hagency_core::project::hash(&serde_json::to_vec(change)?),
        }).await
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests;

#[cfg(test)]
mod parser_tests;
