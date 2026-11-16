//! One conformance check run against every runtime: whatever the model, the
//! turn carries the room's skills (with a use-it instruction) and memory
//! inline, and the room history tool is on the runtime's tool surface.
use super::*;
use crate::room_memory::Entry;
use crate::room_skills::{Skill, inline::INLINE_HEADER};
use crate::room_tools::{ROOM_HISTORY_TOOL, RoomOperation, RoomTools};
use serde_json::json;

#[derive(Debug, Clone, Copy)]
enum Runtime {
    Claude,
    Codex,
}
const RUNTIMES: [Runtime; 2] = [Runtime::Claude, Runtime::Codex];

/// The smallest room tool set each runtime is handed (Claude: shared
/// execution's history-only set; Codex: owned_runtime's native MCP bridge).
fn tools(runtime: Runtime) -> RoomTools {
    match runtime {
        Runtime::Claude => RoomTools::history(),
        Runtime::Codex => RoomTools::history_and_files().with_memory().with_skills(),
    }
}
/// The instructions a runtime's turn starts with around the request.
fn turn_text(runtime: Runtime, context: &str, request: &str) -> String {
    match runtime {
        Runtime::Claude => format!("{}\n\n{context}\n\n{request}", crate::claude::AGENT_TOOLS_NOTE),
        Runtime::Codex => format!("{context}\n\n{request}"),
    }
}

#[test]
fn every_runtime_gets_skills_memory_and_history() {
    let memory = [Entry { id: "m1".into(), text: "Practice repo is acme/demo".into(), author: "@amy:x".into(),
        source_event: None, pinned: true, rev: 1 }];
    let skills = [Skill { name: "release-notes".into(), description: "Write release notes".into(),
        body: "Three emoji bullets, end with thanks.".into(), author: "@amy:x".into(), rev: 1 }];
    let context = compose(&memory, Some(&skills)).expect("context");
    for runtime in RUNTIMES {
        let text = turn_text(runtime, &context, "write the release notes");
        assert!(text.contains(INLINE_HEADER), "{runtime:?}: skill use instruction");
        assert!(text.contains("> release-notes: Write release notes"), "{runtime:?}: skill listed");
        assert!(text.contains("Three emoji bullets, end with thanks."), "{runtime:?}: body inline");
        assert!(text.contains("Practice repo is acme/demo"), "{runtime:?}: memory inline");
        let tools = tools(runtime);
        assert!(tools.definitions().iter().any(|d| d["name"] == ROOM_HISTORY_TOOL), "{runtime:?}: history listed");
        assert!(matches!(tools.parse(ROOM_HISTORY_TOOL, &json!({"cursor":null})), Ok(RoomOperation::History { .. })),
            "{runtime:?}: history callable");
    }
}

#[test]
fn a_room_without_skills_still_says_so_and_keeps_memory() {
    let context = compose(&[], Some(&[])).expect("note");
    assert_eq!(context, crate::room_skills::NO_SKILLS_NOTE);
    assert_eq!(compose(&[], None), None);
}
