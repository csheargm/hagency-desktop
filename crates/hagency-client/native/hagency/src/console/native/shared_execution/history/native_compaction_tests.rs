//! Claude Code owns context compaction; Matrix still owns bounded pages,
//! cursor sequencing, exact coverage and event/fragment deduplication.
use super::*;

#[tokio::test]
async fn room_history_native_prompt_leaves_earlier_pages_for_explicit_reads() {
    let mut reader = super::tests::paged_reader();
    reader.use_native_compaction();
    let input = reader.followup_prompt("Only this new request".into());
    assert_eq!(input.matches("Only this new request").count(), 1);
    assert!(!input.contains("长记录"));
    assert!(!reader.started);
    assert!(input.contains("fresh session"));
    let first: Value = serde_json::from_str(&reader.read(None).await.unwrap()).unwrap();
    assert!(!first["messages"].as_array().unwrap().is_empty());
    assert!(first.get("alreadyRead").is_none());
}

#[tokio::test]
async fn room_history_native_compaction_reads_thousand_messages_without_cumulative_ceiling() {
    let mut reader = super::tests::paged_reader();
    reader.use_native_compaction();
    // A long prompt or earlier pages cannot strand this native context.
    reader.context_remaining = 0;
    let mut cursor = None;
    let mut ids = HashSet::new();
    let mut bytes = 0;
    let mut complete = false;
    for _ in 0..200 {
        let raw = reader.read(cursor.as_deref()).await.unwrap();
        assert!(raw.len() <= hagency_runtime::room_tools::ROOM_HISTORY_RESULT_BYTES);
        bytes += raw.len();
        let page: Value = serde_json::from_str(&raw).unwrap();
        assert!(page.get("reason").is_none());
        for record in page["messages"].as_array().unwrap() {
            let id = record["eventId"].as_str().unwrap();
            assert_ne!(id, "$current");
            assert!(ids.insert(id.to_owned()), "duplicate event {id}");
            assert_eq!(record["body"], "长记录🙂 ".repeat(20));
        }
        let replay: Value =
            serde_json::from_str(&reader.read(cursor.as_deref()).await.unwrap()).unwrap();
        assert_eq!(replay["alreadyRead"], true);
        assert_eq!(replay["messages"], json!([]));
        assert_eq!(replay["nextCursor"], page["nextCursor"]);
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            complete = page["complete"] == true;
            break;
        }
    }
    assert!(complete);
    assert_eq!(ids.len(), 1000);
    assert!(bytes > 256 * 1024);
    assert!(reader.read(Some("history_99999")).await.is_err());
    assert!(reader.read(Some("!foreign:example.test")).await.is_err());
}

struct Document {
    reads: std::sync::atomic::AtomicUsize,
    complete: bool,
}
fn document_text() -> String {
    "PDF 正文🙂\n\u{0001}".repeat(4000)
}
impl HistoryIo for Document {
    async fn request(&self, room: &str, event: &str) -> Result<Option<Value>, NativeError> {
        assert_eq!(room, "!admitted:example.test");
        assert_eq!(event, "$file");
        Ok(Some(
            json!({"eventId":"$file","body":"Already in the current request","attachment":{
            "filename":"设计.pdf","status":"not_read"}}),
        ))
    }
    async fn start(&self, _: &str, _: &str) -> Result<Option<String>, NativeError> {
        Ok(None)
    }
    async fn page(&self, _: &str, _: &str) -> Result<HistoryBatch, NativeError> {
        unreachable!()
    }
    async fn document(
        &self,
        room: &str,
        event: &str,
    ) -> Result<crate::documents::Text, &'static str> {
        assert_eq!(room, "!admitted:example.test");
        assert_eq!(event, "$file");
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut text = crate::documents::Text::plain(&document_text());
        text.complete = self.complete;
        Ok(text)
    }
}

#[tokio::test]
async fn room_history_native_current_attachment_is_discovered_without_repeating_the_request() {
    let source = Arc::new(Document {
        reads: 0.into(),
        complete: true,
    });
    let mut reader = Reader::from_source(source, "!admitted:example.test", "$file");
    reader.use_native_compaction();
    let raw = reader.read(None).await.unwrap();
    assert!(!raw.contains("Already in the current request"));
    let first: Value = serde_json::from_str(&raw).unwrap();
    let cursor = first["requestAttachment"]["attachment"]["readCursor"]
        .as_str()
        .unwrap();
    assert_eq!(first["messages"], json!([]));
    let text: Value = serde_json::from_str(&reader.read(Some(cursor)).await.unwrap()).unwrap();
    assert!(text["text"].as_str().unwrap().contains("PDF 正文"));
    let replay: Value = serde_json::from_str(&reader.read(None).await.unwrap()).unwrap();
    assert_eq!(replay["alreadyRead"], true);
    assert!(replay.get("requestAttachment").is_none());
}

#[tokio::test]
async fn room_history_native_compaction_reads_long_document_and_preserves_incomplete_extraction() {
    for complete in [true, false] {
        let source = Arc::new(Document {
            reads: 0.into(),
            complete,
        });
        let mut reader = Reader::from_source(source.clone(), "!admitted:example.test", "$file");
        reader.use_native_compaction();
        reader.context_remaining = 0;
        let metadata = reader
            .request_attachment(json!({"eventId":"$file","attachment":{
            "filename":"设计.pdf","status":"not_read"}}))
            .unwrap();
        let mut cursor = metadata["attachment"]["readCursor"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut text = String::new();
        let mut ended = false;
        for _ in 0..200 {
            let raw = reader.read(Some(&cursor)).await.unwrap();
            assert!(raw.len() <= hagency_runtime::room_tools::ROOM_HISTORY_RESULT_BYTES);
            let page: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(page["bodyOffsetBytes"], text.len());
            text.push_str(page["text"].as_str().unwrap());
            let replay: Value =
                serde_json::from_str(&reader.read(Some(&cursor)).await.unwrap()).unwrap();
            assert_eq!(replay["alreadyRead"], true);
            assert_eq!(replay["text"], "");
            assert_eq!(replay["nextCursor"], page["nextCursor"]);
            match page["nextCursor"].as_str() {
                Some(next) => cursor = next.into(),
                None => {
                    assert_eq!(page["complete"], complete);
                    assert_eq!(page["extractionComplete"], complete);
                    ended = true;
                    break;
                }
            }
        }
        assert!(ended);
        assert!(text.len() > 32 * 1024);
        assert_eq!(text, document_text());
        assert_eq!(source.reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(reader.read(Some("attachment_99_0")).await.is_err());
        assert!(reader.read(Some("attachment_0_999999")).await.is_err());
    }
}
