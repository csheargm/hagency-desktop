//! Cold resume in native Codex 0.167 has no raw-response subscription option.
//! Read its original per-response rollout records before emitting final usage.
use super::*;

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    pub(super) async fn recover_native_usage(&mut self) {
        if self.legacy_usage_allowed
            || self.native_usage_checked
            || self.phase() != Phase::Ended
            || self.native_rollout.is_none()
            || self
                .tree
                .as_ref()
                .is_none_or(|t| !t.finished || t.graph.usage().is_some())
        {
            return;
        }
        self.native_usage_checked = true;
        // This is bounded final receipt IO, after the native work is finished.
        // Failure never fabricates a charge or prevents delivery of the answer.
        let result = tokio::time::timeout(Duration::from_secs(5), self.read_native_usage()).await;
        if !matches!(result, Ok(Ok(()))) {
            if let Some(tree) = &mut self.tree {
                tree.graph.invalidate_native_usage();
            }
            tracing::warn!("Native completed response receipts remain unavailable");
        }
    }
    async fn read_native_usage(&mut self) -> Result<(), Error> {
        let threads = self
            .tree
            .as_ref()
            .and_then(|tree| tree.graph.completed_native_threads())
            .ok_or(Error::State)?;
        for thread in threads {
            if self.thread_id() == Some(thread.as_str()) {
                self.native_rollout
                    .as_mut()
                    .ok_or(Error::State)?
                    .apply(&mut self.tree.as_mut().ok_or(Error::State)?.graph)
                    .await?;
            } else {
                // Only a descendant already accepted on this original native
                // stream can select a metadata read or a persisted usage file.
                let reply = self
                    .call(
                        "thread/read",
                        serde_json::json!({"threadId":thread,"includeTurns":false}),
                    )
                    .await?;
                let mut cursor =
                    crate::codex::NativeUsageCursor::from_thread_reply(&reply, &thread, false)
                        .await?;
                cursor
                    .apply(&mut self.tree.as_mut().ok_or(Error::State)?.graph)
                    .await?;
            }
        }
        // Metadata RPCs can pump original notifications while awaiting their
        // replies. Apply those before declaring usage complete; a later child
        // turn or contradictory receipt cannot be hidden in the deferred queue.
        self.drain_terminal().await?;
        // A gap in any descendant is unknown, even when other records exist.
        self.tree
            .as_ref()
            .and_then(|tree| tree.graph.usage())
            .ok_or(Error::State)?;
        Ok(())
    }
}
