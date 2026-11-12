//! Shared operator task-graph domain, extracted from the retained console.
//!
//! Graph IDs, name-addressed dispatch messages, dependency conditions and
//! cancellation history keep the original document-store contract. The console
//! is one transport adapter; this module does not start a listener.
//!
//! Document reads retain the private-file policy and use document-sized bounds.
//! Existing invalid or unreadable files must not become empty writable history.
//! Native-owner adoption still needs explicit identity and custody validation;
//! this domain does not authorize mini-app access or migrate documents.
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(feature = "resource-adoption")]
pub(crate) mod canonical;
#[cfg(feature = "resource-adoption")]
pub(crate) mod current;
#[cfg(feature = "resource-adoption")]
pub(crate) mod recipients;
pub(crate) mod transitions;

const GRAPHS_FILE: &str = "task_graphs.json";
const MESSAGES_FILE: &str = "messages.json";
const COUNTER_FILE: &str = ".msg_counter";
const MAX_RESULT_BYTES: usize = 65_536;
const MAX_GRAPH_BYTES: usize = 4 * 1024 * 1024;
const MAX_MESSAGES_BYTES: usize = 64 * 1024 * 1024;

/// One persisted task graph — the TS `normalizeGraph` shape
/// (`lib/task-graph.js:196-216`): camelCase timestamps, `nodes` keyed by id.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Graph {
    pub(crate) id: String,
    owner: String,
    label: String,
    status: String,
    nodes: BTreeMap<String, Node>,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(rename = "completedAt")]
    completed_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Node {
    id: String,
    assignee: String,
    description: String,
    #[serde(default)]
    depends_on: Vec<String>,
    status: String,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    condition: Option<Value>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(rename = "dispatchedAt", default)]
    dispatched_at: Option<String>,
    #[serde(rename = "completedAt", default)]
    completed_at: Option<String>,
    #[serde(rename = "startedAt", default)]
    started_at: Option<String>,
}

pub(crate) fn graph_statuses() -> &'static [&'static str] {
    &["active", "complete", "failed", "cancelled"]
}
fn node_statuses() -> &'static [&'static str] {
    &[
        "pending",
        "dispatched",
        "active",
        "complete",
        "failed",
        "skipped",
        "cancelled",
    ]
}
fn terminal_node(status: &str) -> bool {
    matches!(status, "complete" | "failed" | "skipped" | "cancelled")
}

/// TS `createGraphError` (`lib/task-graph.js:13`): the route maps the code
/// through `taskGraphErrorStatus` (`backend-v2.js:3961-3974`).
#[derive(Debug)]
pub(crate) struct GraphError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}
pub(crate) fn graph_error(code: &'static str, message: impl Into<String>) -> GraphError {
    GraphError {
        code,
        message: message.into(),
    }
}

pub(crate) fn text(value: &Value, max: usize) -> Option<String> {
    let trimmed = value.as_str()?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut out = trimmed.chars();
    let collected: String = out.by_ref().take(max).collect();
    Some(collected)
}

fn string_array(value: Option<&Value>, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(Value::Array(items)) = value {
        for item in items {
            if let Some(text) = text(item, max)
                && seen.insert(text.clone())
            {
                out.push(text);
            }
        }
    }
    out
}

fn normalize_graph_status(value: Option<&Value>) -> Result<String, GraphError> {
    let normalized = value
        .and_then(|v| text(v, 32))
        .unwrap_or_else(|| "active".into());
    if !graph_statuses().contains(&normalized.as_str()) {
        return Err(graph_error(
            "invalid_graph_status",
            format!(
                "invalid graph status: {}",
                value.and_then(Value::as_str).unwrap_or("")
            ),
        ));
    }
    Ok(normalized)
}
fn normalize_node_status(value: Option<&Value>, fallback: &str) -> Result<String, GraphError> {
    let normalized = value
        .and_then(|v| text(v, 32))
        .unwrap_or_else(|| fallback.into());
    if !node_statuses().contains(&normalized.as_str()) {
        return Err(graph_error(
            "invalid_node_status",
            format!(
                "invalid node status: {}",
                value.and_then(Value::as_str).unwrap_or("")
            ),
        ));
    }
    Ok(normalized)
}

fn normalize_result(value: Option<&Value>) -> Result<Value, GraphError> {
    let Some(value) = value else {
        return Ok(Value::Null);
    };
    let bytes =
        serde_json::to_vec(value).map_err(|_| graph_error("invalid_nodes", "invalid result"))?;
    if bytes.len() > MAX_RESULT_BYTES {
        return Err(graph_error(
            "result_too_large",
            format!("result exceeds {MAX_RESULT_BYTES} bytes when serialized"),
        ));
    }
    Ok(value.clone())
}

fn iso_now() -> String {
    // The retained store stamps ISO-8601 UTC (`new Date().toISOString()`).
    // Produce the same shape from the epoch without a date crate: seconds and
    // civil date via the civil-from-days algorithm.
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    iso_from_millis(millis)
}
fn iso_from_millis(millis: u64) -> String {
    let secs = millis / 1000;
    let ms = millis % 1000;
    let days = (secs / 86_400) as i64;
    let day_secs = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        day_secs / 3600,
        (day_secs % 3600) / 60,
        day_secs % 60,
    )
}
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
fn normalize_timestamp(value: Option<&Value>, fallback: Option<String>) -> Option<String> {
    let candidate = value.and_then(|v| text(v, 128))?;
    // TS `Date.parse` accepts any parseable date; we accept only ISO-8601 UTC
    // (the format this service itself emits) and otherwise keep the fallback.
    if candidate.len() == 24 && candidate.ends_with('Z') && candidate.as_bytes()[4] == b'-' {
        Some(candidate)
    } else {
        fallback
    }
}

fn normalize_condition(value: Option<&Value>) -> Result<Option<Value>, GraphError> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(graph_error(
            "invalid_condition",
            "condition must be an object",
        ));
    };
    let mut out = Map::new();
    for key in ["dep", "path", "field", "op"] {
        if let Some(text) = object.get(key).and_then(|v| text(v, 512)) {
            out.insert(
                if key == "field" {
                    "path".into()
                } else {
                    key.into()
                },
                Value::String(text),
            );
        }
    }
    for key in ["eq", "neq", "in", "value"] {
        if let Some(v) = object.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Object(out)))
    }
}

fn normalize_node(id_key: &str, raw: &Value) -> Result<Node, GraphError> {
    let id = text(raw.get("id").unwrap_or(&Value::Null), 255)
        .or_else(|| text(&Value::String(id_key.into()), 255))
        .ok_or_else(|| graph_error("invalid_node_id", "node id required"))?;
    let assignee = text(raw.get("assignee").unwrap_or(&Value::Null), 255).ok_or_else(|| {
        graph_error(
            "invalid_node_assignee",
            format!("node '{id}' assignee required"),
        )
    })?;
    let description =
        text(raw.get("description").unwrap_or(&Value::Null), 4000).ok_or_else(|| {
            graph_error(
                "invalid_node_description",
                format!("node '{id}' description required"),
            )
        })?;
    let has_result = raw.get("result").is_some();
    Ok(Node {
        id: id.clone(),
        assignee,
        description,
        depends_on: string_array(raw.get("depends_on"), 255),
        status: normalize_node_status(raw.get("status"), "pending")?,
        result: if has_result {
            normalize_result(raw.get("result"))?
        } else {
            Value::Null
        },
        error: raw.get("error").and_then(|v| text(v, 4000)),
        condition: normalize_condition(raw.get("condition"))?,
        message_id: raw.get("message_id").and_then(|v| text(v, 255)),
        dispatched_at: normalize_timestamp(
            raw.get("dispatchedAt").or_else(|| raw.get("dispatched_at")),
            None,
        ),
        completed_at: normalize_timestamp(
            raw.get("completedAt").or_else(|| raw.get("completed_at")),
            None,
        ),
        started_at: normalize_timestamp(
            raw.get("startedAt").or_else(|| raw.get("started_at")),
            None,
        ),
    })
}

fn normalize_nodes(raw: Option<&Value>) -> Result<BTreeMap<String, Node>, GraphError> {
    let Some(Value::Object(entries)) = raw else {
        return Err(graph_error("invalid_nodes", "nodes must be an object"));
    };
    if entries.is_empty() {
        return Err(graph_error(
            "invalid_nodes",
            "graph must contain at least one node",
        ));
    }
    let mut out = BTreeMap::new();
    for (key, raw_node) in entries {
        let node = normalize_node(key, raw_node)?;
        out.insert(node.id.clone(), node);
    }
    for node in out.values() {
        for dep in &node.depends_on {
            if !out.contains_key(dep) {
                return Err(graph_error(
                    "invalid_dependency",
                    format!("node '{}' depends on missing node '{dep}'", node.id),
                ));
            }
            if dep == &node.id {
                return Err(graph_error(
                    "invalid_dependency",
                    format!("node '{}' cannot depend on itself", node.id),
                ));
            }
        }
        if let Some(condition) = &node.condition
            && let Some(dep) = condition.get("dep").and_then(Value::as_str)
            && !dep.is_empty()
            && !out.contains_key(dep)
        {
            return Err(graph_error(
                "invalid_condition",
                format!(
                    "node '{}' condition references missing dep '{dep}'",
                    node.id
                ),
            ));
        }
    }
    // Cycle detection, TS `visit` (`lib/task-graph.js:137-153`).
    fn visit(
        id: &str,
        nodes: &BTreeMap<String, Node>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), GraphError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.into()) {
            return Err(graph_error(
                "invalid_dependency_cycle",
                format!("dependency cycle detected at node '{id}'"),
            ));
        }
        for dep in &nodes[id].depends_on {
            visit(dep, nodes, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id.into());
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for id in out.keys() {
        visit(id, &out, &mut visiting, &mut visited)?;
    }
    Ok(out)
}

fn normalize_graph(raw: &Value, id_fallback: Option<String>) -> Result<Graph, GraphError> {
    let id = text(raw.get("id").unwrap_or(&Value::Null), 255)
        .or(id_fallback)
        .ok_or_else(|| graph_error("invalid_nodes", "graph id required"))?;
    let owner = text(raw.get("owner").unwrap_or(&Value::Null), 255)
        .ok_or_else(|| graph_error("invalid_graph_owner", "graph owner required"))?;
    let label = text(raw.get("label").unwrap_or(&Value::Null), 4000)
        .ok_or_else(|| graph_error("invalid_graph_label", "graph label required"))?;
    let created_at = normalize_timestamp(
        raw.get("createdAt").or_else(|| raw.get("created_at")),
        Some(iso_now()),
    )
    .unwrap_or_else(iso_now);
    let updated_at = normalize_timestamp(
        raw.get("updatedAt").or_else(|| raw.get("updated_at")),
        Some(created_at.clone()),
    )
    .unwrap_or_else(|| created_at.clone());
    let completed_at = normalize_timestamp(
        raw.get("completedAt").or_else(|| raw.get("completed_at")),
        None,
    );
    Ok(Graph {
        id,
        owner,
        label,
        status: normalize_graph_status(raw.get("status"))?,
        nodes: normalize_nodes(raw.get("nodes"))?,
        created_at,
        updated_at,
        completed_at,
    })
}

fn default_graph_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut suffix = [0u8; 4];
    let _ = getrandom::fill(&mut suffix);
    let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let tail: String = suffix
        .iter()
        .map(|b| alphabet[(*b as usize) % 36] as char)
        .collect();
    // TS `graph_${Date.now()}_${random base36(6)}`.
    format!("graph_{millis}_{tail}")
}

/// The whole store: a map plus the state dir, mirroring TS's in-memory map
/// backed by `task_graphs.json`.
pub(crate) struct GraphStore {
    dir: PathBuf,
    inner: std::sync::Mutex<StoreInner>,
    // Retain the existing installation-wide writer admission through every
    // graph, message and counter write, including an in-flight HTTP request.
    // Unsupported platforms do not claim installation-wide lock coverage.
    _writer: hagency_platform::legacy_writer::LegacyWriterAdmission,
}
struct StoreInner {
    graphs: BTreeMap<String, Graph>,
    messages: Vec<Value>,
    counter: u64,
}

impl GraphStore {
    pub(crate) fn open(dir: &Path) -> Result<Self, GraphError> {
        let writer =
            hagency_platform::legacy_writer::LegacyWriterAdmission::acquire()
                .map_err(|_| {
                    graph_error(
                        "graph_writer_unavailable",
                        "task graph writer is unavailable",
                    )
                })?;
        Self::with_writer(dir, writer)
    }

    fn with_writer(
        dir: &Path,
        writer: hagency_platform::legacy_writer::LegacyWriterAdmission,
    ) -> Result<Self, GraphError> {
        let graphs =
            read_json(&dir.join(GRAPHS_FILE), MAX_GRAPH_BYTES)?.unwrap_or_else(|| json!({}));
        let messages =
            read_json(&dir.join(MESSAGES_FILE), MAX_MESSAGES_BYTES)?.unwrap_or_else(|| json!([]));
        let counter = read_json(&dir.join(COUNTER_FILE), 512)?.unwrap_or_else(|| json!(0));
        let mut map = BTreeMap::new();
        let entries = graphs.as_object().ok_or_else(document_error)?;
        for (key, raw) in entries {
            let graph = normalize_graph(raw, Some(key.clone()).filter(|k| !k.is_empty()))?;
            if map.insert(graph.id.clone(), graph).is_some() {
                return Err(document_error());
            }
        }
        let messages_vec = messages.as_array().cloned().ok_or_else(document_error)?;
        // TS boot reconciles the counter against persisted message ids
        // (`backend-v2.js:2829-2834`).
        let mut counter = counter.as_u64().ok_or_else(document_error)?;
        for message in &messages_vec {
            if let Some(n) = message
                .get("id")
                .and_then(Value::as_str)
                .and_then(message_counter_from_id)
            {
                counter = counter.max(n);
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            inner: std::sync::Mutex::new(StoreInner {
                graphs: map,
                messages: messages_vec,
                counter,
            }),
            _writer: writer,
        })
    }

    fn persist_graphs(&self, graphs: &BTreeMap<String, Graph>) -> Result<(), GraphError> {
        let value = serde_json::to_value(graphs).map_err(|_| {
            graph_error("graph_persistence_failed", "task graph persistence failed")
        })?;
        if serde_json::to_vec(&value).map(|v| v.len()).unwrap_or(0) > MAX_GRAPH_BYTES {
            return Err(graph_error(
                "graph_persistence_failed",
                "task graph persistence failed",
            ));
        }
        hagency_store::private::replace(&self.dir.join(GRAPHS_FILE), value.to_string().as_bytes())
            .map_err(|_| graph_error("graph_persistence_failed", "task graph persistence failed"))
    }
    fn persist_messages(&self, messages: &[Value]) -> Result<(), GraphError> {
        let value = Value::Array(messages.to_vec());
        let bytes = serde_json::to_vec(&value).map_err(|_| document_error())?;
        if bytes.len() > MAX_MESSAGES_BYTES {
            return Err(graph_error(
                "graph_dispatch_failed",
                "message document exceeds its bound",
            ));
        }
        hagency_store::private::replace(&self.dir.join(MESSAGES_FILE), &bytes)
            .map_err(|_| graph_error("graph_dispatch_failed", "message persistence failed"))
    }
    fn persist_counter(&self, counter: u64) -> Result<(), GraphError> {
        hagency_store::private::replace(
            &self.dir.join(COUNTER_FILE),
            counter.to_string().as_bytes(),
        )
        .map_err(|_| graph_error("graph_dispatch_failed", "msg_counter persistence failed"))
    }

    /// TS `dispatchTaskGraphMessage` -> `dispatchInternalDirectMessage`:
    /// one durable, name-addressed message with the `task_graph_dispatch`
    /// schema, deduped on the dispatch key. Returns the message id.
    fn dispatch(
        &self,
        inner: &mut StoreInner,
        graph: &Graph,
        node: &Node,
    ) -> Result<String, GraphError> {
        let dispatch_key = transitions::dispatch_key(graph, node);
        if let Some(existing) = inner.messages.iter().rev().find(|m| {
            m.get("schema")
                .and_then(|s| s.get("payload"))
                .and_then(|p| p.get("dispatchKey"))
                .and_then(Value::as_str)
                == Some(dispatch_key.as_str())
        }) {
            return existing
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    graph_error(
                        "graph_dispatch_failed",
                        "task graph dispatch did not return a durable message id",
                    )
                });
        }
        let next = inner
            .counter
            .checked_add(1)
            .ok_or_else(|| graph_error("graph_dispatch_failed", "message counter is exhausted"))?;
        self.persist_counter(next)?;
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let message = transitions::dispatch_message(
            graph,
            node,
            &format!("msg_{next:04}"),
            timestamp_ms,
            &view_token(),
        );
        let mut messages = inner.messages.clone();
        messages.push(message.clone());
        self.persist_messages(&messages)?;
        inner.counter = next;
        inner.messages = messages;
        message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                graph_error(
                    "graph_dispatch_failed",
                    "task graph dispatch did not return a durable message id",
                )
            })
    }

    pub(crate) fn create(&self, raw: &Value) -> Result<Graph, GraphError> {
        let mut inner = self.inner.lock().map_err(|_| {
            graph_error("graph_persistence_failed", "task graph persistence failed")
        })?;
        let graph = normalize_graph(raw, raw.get("id").is_none().then_some(default_graph_id()))?;
        if inner.graphs.contains_key(&graph.id) {
            return Err(graph_error(
                "graph_exists",
                format!("graph already exists: {}", graph.id),
            ));
        }
        let mut next = inner.graphs.clone();
        next.insert(graph.id.clone(), graph.clone());
        self.persist_graphs(&next)?;
        inner.graphs = next;
        Ok(graph)
    }

    pub(crate) fn get(&self, id: &str) -> Option<Graph> {
        self.inner.lock().ok()?.graphs.get(id).cloned()
    }
    pub(crate) fn list(&self, status: Option<&str>) -> Vec<Graph> {
        let mut graphs: Vec<Graph> = self
            .inner
            .lock()
            .map(|i| {
                i.graphs
                    .values()
                    .filter(|g| status.is_none_or(|s| g.status == s))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        graphs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        graphs
    }

    /// TS `deleteGraph` (`lib/task-graph.js:421`): a cancel, not a removal.
    pub(crate) fn delete(&self, id: &str) -> Result<Option<Graph>, GraphError> {
        let mut inner = self.inner.lock().map_err(|_| {
            graph_error("graph_persistence_failed", "task graph persistence failed")
        })?;
        let Some(graph) = inner.graphs.get(id) else {
            return Ok(None);
        };
        let transition = transitions::cancel(graph, &iso_now());
        if transition.changed {
            let mut next = inner.graphs.clone();
            next.insert(id.into(), transition.graph);
            self.persist_graphs(&next)?;
            inner.graphs = next;
        }
        Ok(inner.graphs.get(id).cloned())
    }

    /// TS `updateNode` (`lib/task-graph.js:455`).
    pub(crate) fn update_node(
        &self,
        id: &str,
        node_id: &str,
        patch: &Value,
    ) -> Result<(Graph, Node), GraphError> {
        let mut inner = self.inner.lock().map_err(|_| {
            graph_error("graph_persistence_failed", "task graph persistence failed")
        })?;
        let graph = inner
            .graphs
            .get(id)
            .ok_or_else(|| graph_error("graph_not_found", format!("graph not found: {id}")))?;
        let transition = transitions::patch_node(graph, node_id, patch, &iso_now())?;
        if transition.changed {
            let mut next = inner.graphs.clone();
            next.insert(id.into(), transition.graph);
            self.persist_graphs(&next)?;
            inner.graphs = next;
        }
        let graph = inner.graphs[id].clone();
        let node = graph.nodes[node_id].clone();
        Ok((graph, node))
    }

    /// TS `advanceGraph` (`lib/task-graph.js:522`): dispatch ready roots,
    /// cascade failed dependencies, finalize a terminal graph.
    pub(crate) fn advance(&self, id: &str) -> Result<Option<Graph>, GraphError> {
        let mut inner = self.inner.lock().map_err(|_| {
            graph_error("graph_persistence_failed", "task graph persistence failed")
        })?;
        let Some(graph) = inner.graphs.get(id).cloned() else {
            return Ok(None);
        };
        // The callback preserves the legacy ordering: durable counter, durable
        // message, then graph state. Failed graph writes can be retried using
        // the existing dispatch key without creating a second message.
        let transition = transitions::advance(&graph, &iso_now(), |graph, node| {
            self.dispatch(&mut inner, graph, node)
        })?;
        if transition.changed {
            let mut next = inner.graphs.clone();
            next.insert(id.into(), transition.graph);
            self.persist_graphs(&next)?;
            inner.graphs = next;
        }
        Ok(inner.graphs.get(id).cloned())
    }
}

fn nested<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty()
        || path
            .split('.')
            .any(|part| matches!(part, "__proto__" | "constructor" | "prototype"))
    {
        return None;
    }
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// TS `evaluateCondition` (`lib/task-graph.js:173`): None = not yet decidable,
/// Some(false) = skip, Some(true) = dispatch.
fn evaluate_condition(graph: &Graph, node: &Node) -> Option<bool> {
    // TS `if (!node?.condition) return true;` — no condition always passes.
    let Some(condition) = node.condition.as_ref() else {
        return Some(true);
    };
    let dep_id = condition
        .get("dep")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| node.depends_on.first().cloned())?;
    let dep = graph.nodes.get(&dep_id)?;
    match dep.status.as_str() {
        "pending" | "dispatched" | "active" => return None,
        "complete" => {}
        _ => return Some(false),
    }
    let path = condition
        .get("path")
        .or_else(|| condition.get("field"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let value = path
        .and_then(|p| nested(&dep.result, p))
        .cloned()
        .unwrap_or_else(|| dep.result.clone());
    if let Some(eq) = condition.get("eq") {
        return Some(&value == eq);
    }
    if let Some(neq) = condition.get("neq") {
        return Some(&value != neq);
    }
    if let Some(set) = condition.get("in") {
        return Some(set.as_array().is_some_and(|a| a.contains(&value)));
    }
    match condition.get("op").and_then(Value::as_str) {
        Some("eq") => return Some(condition.get("value").is_some_and(|v| &value == v)),
        Some("neq") => return Some(condition.get("value").is_none_or(|v| &value != v)),
        Some("in") => {
            return Some(
                condition
                    .get("value")
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.contains(&value)),
            );
        }
        _ => {}
    }
    Some(!value.is_null() && value != Value::Bool(false))
}

fn view_token() -> String {
    // TS `createMessageViewToken` = 24 random bytes, base64url.
    let mut bytes = [0u8; 24];
    let _ = getrandom::fill(&mut bytes);
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(32);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        }
    }
    out
}

fn message_counter_from_id(id: &str) -> Option<u64> {
    let rest = id.strip_prefix("msg_")?;
    let value: u64 = rest.parse().ok()?;
    (value > 0).then_some(value)
}

fn document_error() -> GraphError {
    graph_error(
        "graph_persistence_failed",
        "task graph history is invalid or unreadable",
    )
}

fn read_json(path: &Path, limit: usize) -> Result<Option<Value>, GraphError> {
    let file = match hagency_store::private::open(path, false) {
        Ok(file) => file,
        Err(hagency_store::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(_) => return Err(document_error()),
    };
    if file.metadata().map_err(|_| document_error())?.len() > limit as u64 {
        return Err(document_error());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| document_error())?;
    if bytes.len() > limit {
        return Err(document_error());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| document_error())
}

#[cfg(test)]
mod tests;
