//! Share cold native receipt decoding with the supervised pooled runtime.
use super::*;
use hagency_runtime::codex::{NativeUsageCursor, tree::TurnTree};

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Session<R, W> {
    pub(super) async fn recover_native_usage(&mut self, tree: &mut TurnTree) {
        if self.legacy_usage || self.native_rollout.is_none() || tree.usage().is_some() {
            return;
        }
        // This bounds final receipt IO, never native reasoning or tool work.
        // Keep the completed answer when a file, metadata read or stream fails.
        let result =
            tokio::time::timeout(Duration::from_secs(5), self.read_native_usage(tree)).await;
        if !matches!(result, Ok(Ok(()))) {
            tree.invalidate_native_usage();
        }
    }

    async fn read_native_usage(&mut self, tree: &mut TurnTree) -> Result<()> {
        let threads = tree.completed_native_threads().ok_or(Error::Unknown)?;
        for thread in threads {
            if self.thread.as_deref() == Some(thread.as_str()) {
                self.native_rollout
                    .as_mut()
                    .ok_or(Error::Unknown)?
                    .apply(tree)
                    .await
                    .map_err(|_| Error::Unknown)?;
            } else {
                let reply = self
                    .rpc_with_idle(
                        "thread/read",
                        value!({"threadId":thread,"includeTurns":false}),
                        true,
                    )
                    .await?;
                let mut cursor = NativeUsageCursor::from_thread_reply(&reply, &thread, false)
                    .await
                    .map_err(|_| Error::Unknown)?;
                cursor.apply(tree).await.map_err(|_| Error::Unknown)?;
            }
        }
        // Metadata reads may queue original notifications. A contradictory
        // receipt or new child work must not disappear behind the RPC reply.
        while !self.deferred.is_empty()
            || !self.reader.buffer().is_empty()
            || !self.read_frame.is_empty()
        {
            let frame = match self.deferred.pop_front() {
                Some(frame) => frame,
                None => self.read().await?,
            };
            if frame.get("id").is_some() {
                self.write(value!({"id":frame["id"],"error":{"code":-32601,"message":"No active scoped approval"}})).await?;
                return Err(Error::Unknown);
            }
            let method = frame["method"].as_str().ok_or(Error::Unknown)?;
            tree.observe(method, &frame["params"], frame.get("id").is_some())
                .map_err(|_| Error::Unknown)?;
        }
        tree.usage().ok_or(Error::Unknown)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests;
