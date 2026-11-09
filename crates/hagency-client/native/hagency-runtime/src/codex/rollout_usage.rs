//! Original native per-response records. Context totals are never substituted
//! for receipts when a cold-resumed app-server does not emit raw response events.
use super::session::Error;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};

const MAX_LINE: usize = 1024 * 1024;
const MAX_READ: usize = 128 * 1024 * 1024;
const MAX_RECEIPTS: usize = 16_384;

pub(super) struct Receipt {
    pub(super) thread: String,
    pub(super) turn: String,
    pub(super) root_turn: String,
    pub(super) response: String,
    pub(super) usage: Value,
}
/// Bounded reader of original native per-response records. Like `TurnTree`,
/// this produces protocol observations, never execution or payment authority.
pub struct Cursor {
    reader: BufReader<tokio::fs::File>,
    thread: String,
    offset: u64,
}
impl Cursor {
    /// Use only the original `thread/resume` or `thread/read` response from the
    /// retained native transport. Pin EOF for a root before sending its input;
    /// descendants are read from the start and matched to observed native turns.
    pub async fn from_thread_reply(
        reply: &Value,
        thread: &str,
        at_end: bool,
    ) -> Result<Self, Error> {
        if !super::text(thread, 256) || reply["thread"]["id"] != thread {
            return Err(Error::Scope);
        }
        let path = reply["thread"]["path"].as_str().ok_or(Error::State)?;
        Self::open(path, thread, at_end).await
    }

    /// Apply receipts only for completed turns already accepted on the original
    /// native stream. No caller-supplied counters or reconstructed events enter
    /// the tree. A failure must invalidate the whole call's usage.
    pub async fn apply(&mut self, tree: &mut super::tree::TurnTree) -> Result<(), Error> {
        let result = self.apply_inner(tree).await;
        if result.is_err() {
            tree.invalidate_native_usage();
        }
        result
    }

    async fn apply_inner(&mut self, tree: &mut super::tree::TurnTree) -> Result<(), Error> {
        let (root, mut threads) = tree.completed_native_turns().ok_or(Error::State)?;
        let turns = threads.remove(&self.thread).ok_or(Error::Scope)?;
        for receipt in self.read(&root, &turns).await? {
            tree.observe_rollout_receipt(receipt)?;
        }
        Ok(())
    }

    /// The path and thread come from the original native metadata response.
    /// Before a root turn starts, pin EOF so earlier history is not read again.
    pub(super) async fn open(path: &str, thread: &str, at_end: bool) -> Result<Self, Error> {
        let path = Path::new(path);
        if !path.is_absolute() {
            return Err(Error::Scope);
        }
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| Error::State)?;
        if !file.metadata().await.map_err(|_| Error::State)?.is_file() {
            return Err(Error::Scope);
        }
        let mut reader = BufReader::new(file);
        let line = line(&mut reader).await?.ok_or(Error::State)?;
        let meta: Value = serde_json::from_slice(&line).map_err(|_| Error::Malformed)?;
        if meta["type"] != "session_meta" || meta["payload"]["id"] != thread {
            return Err(Error::Scope);
        }
        let offset = if at_end {
            reader
                .seek(std::io::SeekFrom::End(0))
                .await
                .map_err(|_| Error::State)?
        } else {
            reader.stream_position().await.map_err(|_| Error::State)?
        };
        Ok(Self {
            reader,
            thread: thread.into(),
            offset,
        })
    }
    pub(super) async fn read(
        &mut self,
        root_turn: &str,
        turns: &BTreeSet<String>,
    ) -> Result<Vec<Receipt>, Error> {
        if self
            .reader
            .get_ref()
            .metadata()
            .await
            .map_err(|_| Error::State)?
            .len()
            < self.offset
        {
            return Err(Error::Scope);
        }
        let mut bytes = 0usize;
        let mut receipts = Vec::new();
        let mut completed = BTreeSet::new();
        let mut totals = BTreeMap::<String, super::tree::ResponseTotals>::new();
        let mut seen = BTreeMap::<String, (String, Value, Value)>::new();
        while let Some(line) = line(&mut self.reader).await? {
            bytes = bytes.checked_add(line.len()).ok_or(Error::Capacity)?;
            if bytes > MAX_READ {
                return Err(Error::Capacity);
            }
            let value: Value = serde_json::from_slice(&line).map_err(|_| Error::Malformed)?;
            if value["type"] == "event_msg"
                && matches!(
                    value["payload"]["type"].as_str(),
                    Some("task_complete" | "turn_complete")
                )
                && let Some(turn) = value["payload"]["turn_id"]
                    .as_str()
                    .filter(|id| turns.contains(*id))
            {
                completed.insert(turn.to_owned());
            }
            if value["type"] != "token_usage_record" {
                continue;
            }
            let p = &value["payload"];
            let turn = p["turn_id"].as_str().ok_or(Error::Malformed)?;
            if !turns.contains(turn) {
                continue;
            }
            if p["thread_id"] != self.thread || p["root_turn_id"] != root_turn {
                return Err(Error::Scope);
            }
            let response = p["response_id"]
                .as_str()
                .filter(|v| super::text(v, 256))
                .ok_or(Error::Malformed)?;
            let usage = counts(&p["usage"]);
            let total = counts(&p["turn_token_usage"]);
            let values = super::tree::ResponseTotals::decode(&usage).ok_or(Error::Malformed)?;
            let cumulative = super::tree::ResponseTotals::decode(&total).ok_or(Error::Malformed)?;
            if let Some(old) = seen.get(response) {
                if old != &(turn.to_owned(), usage, total) {
                    return Err(Error::Scope);
                }
                continue;
            }
            let sum = totals
                .get(turn)
                .copied()
                .unwrap_or_default()
                .add(values)
                .ok_or(Error::Capacity)?;
            if sum != cumulative || completed.contains(turn) {
                return Err(Error::Scope);
            }
            totals.insert(turn.to_owned(), sum);
            seen.insert(response.into(), (turn.into(), usage.clone(), total));
            if receipts.len() >= MAX_RECEIPTS {
                return Err(Error::Capacity);
            }
            receipts.push(Receipt {
                thread: self.thread.clone(),
                turn: turn.into(),
                root_turn: root_turn.into(),
                response: response.into(),
                usage,
            });
        }
        if &completed != turns || turns.iter().any(|turn| !totals.contains_key(turn)) {
            return Err(Error::State);
        }
        self.offset = self
            .reader
            .stream_position()
            .await
            .map_err(|_| Error::State)?;
        Ok(receipts)
    }
}
fn counts(value: &Value) -> Value {
    let mut usage = serde_json::Map::new();
    for (from, to) in [
        ("total_tokens", "totalTokens"),
        ("input_tokens", "inputTokens"),
        ("output_tokens", "outputTokens"),
        ("cached_input_tokens", "cachedInputTokens"),
        ("cache_write_input_tokens", "cacheWriteInputTokens"),
        ("reasoning_output_tokens", "reasoningOutputTokens"),
    ] {
        // The native protocol defaults only cache writes to zero.
        let count = if from == "cache_write_input_tokens" && value.get(from).is_none() {
            Value::from(0)
        } else {
            value[from].clone()
        };
        usage.insert(to.into(), count);
    }
    usage.into()
}
async fn line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, Error> {
    let mut out = Vec::new();
    loop {
        let bytes = reader.fill_buf().await.map_err(|_| Error::State)?;
        if bytes.is_empty() {
            return if out.is_empty() {
                Ok(None)
            } else {
                Err(Error::Malformed)
            };
        }
        let end = bytes.iter().position(|b| *b == b'\n');
        let count = end.map_or(bytes.len(), |i| i + 1);
        if out.len() + count > MAX_LINE {
            return Err(Error::Capacity);
        }
        out.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(out));
        }
    }
}

#[cfg(test)]
#[path = "rollout_usage_tests.rs"]
mod tests;
