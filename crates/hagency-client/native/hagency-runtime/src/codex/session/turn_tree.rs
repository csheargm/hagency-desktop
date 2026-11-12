//! Native descendants remain part of the original admitted Matrix call. Their
//! IDs are retained verbatim, never rewritten into the root's callback scope.
use super::*;
use crate::codex::session::{ObservationKind, UsageEvidence, observation::EvidenceTracker};
use crate::codex::tree::TurnTree;

struct Child {
    state: State,
    evidence: EvidenceTracker,
}
pub(super) struct Streams {
    pub graph: TurnTree,
    pub finished: bool,
    children: BTreeMap<(String, String), Child>,
}
impl Streams {
    pub fn new(thread: &str, turn: &str) -> Result<Self, Error> {
        Ok(Self {
            graph: TurnTree::new(thread, turn)?,
            finished: false,
            children: BTreeMap::new(),
        })
    }
    pub fn notification(
        &mut self,
        root: &mut State,
        evidence: &mut EvidenceTracker,
        legacy_usage_allowed: bool,
        method: &str,
        params: &Value,
    ) -> Result<(Update, ObservationKind), Error> {
        Self::check_scope(&self.graph, root, legacy_usage_allowed, method, params)?;
        let scope = self.graph.observe(method, params, false)?;
        if method.starts_with("codex/event/") || method == "rawResponseItem/completed" {
            return Ok((Update::Notice, ObservationKind::Ignored));
        }
        if method == "rawResponse/completed" {
            return Ok((
                Update::Progress,
                ObservationKind::Usage(UsageEvidence::responses(self.graph.measured_responses())),
            ));
        }
        // Context-history counters on a fork include its parent's history.
        // Only exact per-response receipts can measure a tree. Older runtimes
        // retain the existing fresh-root snapshot path, never a cold resume.
        if method == "thread/tokenUsage/updated"
            && (!legacy_usage_allowed
                || self.graph.has_response_receipts()
                || self.graph.has_descendants()
                || scope.is_none())
        {
            return Ok((Update::Progress, ObservationKind::Ignored));
        }
        let is_root = scope.as_ref().map_or_else(
            || {
                params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .or_else(|| params["thread"]["id"].as_str())
                    .is_none_or(|id| Some(id) == root.thread.as_deref())
            },
            |scope| scope.root,
        );
        let (mut update, mut observation) = if is_root {
            if root.phase == Phase::Ended {
                root.terminal_suffix(method, params)?;
                (Update::Progress, ObservationKind::Ignored)
            } else {
                let update = root.notification(method, params)?;
                let observation = if method == "thread/tokenUsage/updated" {
                    ObservationKind::Usage(crate::codex::session::usage::project(params))
                } else {
                    evidence.project(&update, params, root.outcome.as_ref())
                };
                (update, observation)
            }
        } else if let Some(scope) = scope {
            let child = self
                .children
                .entry((scope.thread.clone(), scope.turn.clone()))
                .or_insert_with(|| {
                    let mut state = State::default();
                    state.thread = Some(scope.thread);
                    state.turn = Some(scope.turn);
                    state.phase = Phase::Running;
                    Child {
                        state,
                        evidence: EvidenceTracker::default(),
                    }
                });
            let update = if child.state.phase == Phase::Ended {
                child.state.terminal_suffix(method, params)?;
                Update::Progress
            } else {
                child.state.notification(method, params)?
            };
            let observation = child
                .evidence
                .project(&update, params, child.state.outcome.as_ref());
            // Keep tool evidence and approval correlation; the child's private
            // text and turn completion do not become the root's answer/end.
            match update {
                Update::TextDelta { .. } | Update::TurnEnded | Update::TurnStarted => (
                    Update::Progress,
                    if matches!(observation, ObservationKind::Invalidated) {
                        observation
                    } else {
                        ObservationKind::Ignored
                    },
                ),
                _ => (update, observation),
            }
        } else {
            // Child startup/status can precede turn/started and carry no turn
            // authority. A resolved callback, however, must name this tree.
            if method == "serverRequest/resolved"
                && !params["threadId"]
                    .as_str()
                    .is_some_and(|thread| self.graph.contains_thread(thread))
            {
                return Err(Error::Scope);
            }
            (Update::Notice, ObservationKind::Ignored)
        };
        let items = root.item_count()
            + self
                .children
                .values()
                .map(|child| child.state.item_count())
                .sum::<usize>();
        let text = root.text_bytes()
            + self
                .children
                .values()
                .map(|child| child.state.text_bytes())
                .sum::<usize>();
        if items > root.max_items || text > MAX_TEXT_BYTES * 16 {
            return Err(Error::Capacity);
        }
        if root.phase == Phase::Ended {
            // The legacy failed-error echo has no descendants to wait for.
            if self.graph.is_complete()
                || (!self.graph.has_descendants() && !self.graph.has_response_receipts())
            {
                self.finished = true;
                update = Update::TurnEnded;
                if !matches!(observation, ObservationKind::Invalidated) {
                    observation = crate::codex::session::observation::project(
                        &update,
                        params,
                        root.outcome.as_ref(),
                    );
                }
            } else if matches!(update, Update::TurnEnded) {
                update = Update::Progress;
                if !matches!(observation, ObservationKind::Invalidated) {
                    observation = ObservationKind::Ignored;
                }
            }
        }
        Ok((update, observation))
    }
    pub fn check_scope(
        graph: &TurnTree,
        root: &State,
        legacy: bool,
        method: &str,
        params: &Value,
    ) -> Result<(), Error> {
        if method.starts_with("codex/event/") || method == "rawResponseItem/completed" {
            return Ok(());
        }
        let thread = params
            .get("threadId")
            .and_then(Value::as_str)
            .or_else(|| params["thread"]["id"].as_str());
        if let Some(thread) = thread
            && Some(thread) != root.thread.as_deref()
            && !graph.contains_thread(thread)
            && method != "turn/started"
            && !(matches!(
                method,
                "thread/started"
                    | "thread/status/changed"
                    | "warning"
                    | "mcpServer/startupStatus/updated"
            ) && graph.awaiting_collaboration())
        {
            return Err(Error::Scope);
        }
        if legacy
            && method == "thread/tokenUsage/updated"
            && thread == root.thread.as_deref()
            && params["turnId"].as_str() != root.turn.as_deref()
        {
            return Err(Error::Scope);
        }
        Ok(())
    }
    pub fn terminal_suffix(
        &mut self,
        root: &mut State,
        method: &str,
        params: &Value,
    ) -> Result<(), Error> {
        Self::check_scope(&self.graph, root, false, method, params)?;
        let scope = self.graph.observe(method, params, false)?;
        if method.starts_with("codex/event/")
            || matches!(
                method,
                "rawResponseItem/completed" | "rawResponse/completed"
            )
        {
            return Ok(());
        }
        if let Some(scope) = scope
            && !scope.root
        {
            return self
                .children
                .get_mut(&(scope.thread, scope.turn))
                .ok_or(Error::Scope)?
                .state
                .terminal_suffix(method, params);
        }
        if params
            .get("threadId")
            .and_then(Value::as_str)
            .is_some_and(|id| Some(id) != root.thread.as_deref())
        {
            return if matches!(method, "thread/status/changed" | "serverRequest/resolved")
                && self
                    .graph
                    .contains_thread(params["threadId"].as_str().ok_or(Error::Scope)?)
            {
                Ok(())
            } else {
                Err(Error::Scope)
            };
        }
        root.terminal_suffix(method, params)
    }
}
impl<R, W, E> SessionDriver<R, W, E> {
    pub(super) fn finish_root_after_eof(&mut self, error: Error) -> Option<Update> {
        if error != Error::Transport(transport::Error::PeerEof)
            || !matches!(self.state.outcome, Some(Outcome::Completed { .. })) {
            return None;
        }
        let tree = self.tree.as_mut()?;
        if tree.finished || tree.graph.is_complete() { return None; }
        // The root already supplied a validated final answer. Lost child IO
        // cannot erase that text or invent a child's missing usage. The tree
        // stays incomplete, so terminal_usage explicitly retains the hold.
        // Whole-process-tree cleanup remains the outer owner's responsibility.
        tree.finished = true;
        self.observation_kind = crate::codex::session::observation::project(
            &Update::TurnEnded, &Value::Null, self.state.outcome.as_ref());
        self.wire.close();
        Some(Update::TurnEnded)
    }
    pub(super) fn active_turn(&self, thread: &str, turn: &str) -> bool {
        self.tree
            .as_ref()
            .is_some_and(|tree| !tree.finished && tree.graph.is_active(thread, turn))
    }
    pub(super) fn callback_scope(&self, params: &Value) -> Result<(), Error> {
        if self.phase() != Phase::Running
            || !self.active_turn(
                params["threadId"].as_str().ok_or(Error::Scope)?,
                params["turnId"].as_str().ok_or(Error::Scope)?,
            )
        {
            return Err(Error::Scope);
        }
        Ok(())
    }
    pub(super) fn terminal_usage(&self) -> Option<UsageEvidence> {
        let tree = self.tree.as_ref()?;
        if !tree.finished
            || (!tree.graph.has_descendants()
                && !tree.graph.has_response_receipts()
                && self.legacy_usage_allowed)
        {
            return None;
        }
        Some(UsageEvidence::responses(
            tree.graph
                .usage()
                .and_then(|_| tree.graph.measured_responses()),
        ))
    }
}
