//! A peer that answers a prompt and fails in the same instant. Every byte of
//! the prompt had left, so its write is confirmed first; the answer is then
//! read, and the failure that follows it is the stream's, not the write's.
use super::*;
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

#[derive(Default)]
struct Script {
    /// What the peer wrote and the host has not read yet.
    out: VecDeque<u8>,
    closed: bool,
    line: Vec<u8>,
    waker: Option<Waker>,
    /// After its answer the peer either exits or writes a frame that cannot be
    /// parsed.
    garbled: bool,
}
struct Stdin(Arc<Mutex<Script>>);
struct Stdout(Arc<Mutex<Script>>);

impl tokio::io::AsyncWrite for Stdin {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut script = self.0.lock().unwrap();
        for &byte in bytes {
            if byte != b'\n' {
                script.line.push(byte);
                continue;
            }
            let frame: Value = serde_json::from_slice(&std::mem::take(&mut script.line)).unwrap();
            // The peer acts the moment a whole frame has arrived: before the
            // host's flush, which is the instant this test is about.
            if frame["type"] == "control_request" {
                script.out.extend(encoded(json!({"type":"control_response","response":{
                    "subtype":"success","request_id":frame["request_id"],
                    "response":{"claude_code_version":"2.1.322"}}})));
            } else if frame["type"] == "user" {
                let mut opened = init();
                opened["user_message_uuids"] = json!([frame["uuid"]]);
                let mut answer = result();
                answer["user_message_uuid"] = frame["uuid"].clone();
                answer["user_message_uuids"] = json!([frame["uuid"]]);
                script.out.extend(encoded(opened));
                script.out.extend(encoded(answer));
                if script.garbled {
                    script.out.extend(b"{\"type\":\"assistant\",\"session_id\":\n");
                } else {
                    script.closed = true;
                }
            }
            if let Some(waker) = script.waker.take() {
                waker.wake();
            }
        }
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl tokio::io::AsyncRead for Stdout {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut script = self.0.lock().unwrap();
        if script.out.is_empty() {
            if script.closed {
                return Poll::Ready(Ok(()));
            }
            script.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let count = buffer.remaining().min(script.out.len());
        let bytes: Vec<u8> = script.out.drain(..count).collect();
        buffer.put_slice(&bytes);
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn native_claude_prompt_is_confirmed_before_a_reply_that_then_fails_is_read() {
    for garbled in [false, true] {
        let script = Arc::new(Mutex::new(Script {
            garbled,
            ..Script::default()
        }));
        let (stderr, _error) = tokio::io::duplex(64);
        let mut driver = SessionDriver::new(
            Stdout(script.clone()),
            Stdin(script.clone()),
            stderr,
            limits(),
        )
        .unwrap();
        driver.enable_native_completion().unwrap();
        driver.enable_continuous_session().unwrap();
        driver.initialize().await.unwrap();
        // The whole reply, and the failure after it, is readable before the
        // prompt's flush.
        let receipt = driver
            .prompt("question")
            .await
            .unwrap_or_else(|error| panic!("garbled={garbled}: a delivered prompt failed: {error:?}"));
        assert_eq!(receipt.accepted_bytes, receipt.total_bytes);
        assert!(receipt.flushed);
        assert_eq!(driver.phase(), Phase::Running);
        assert!(matches!(
            driver.next_message().await.unwrap(),
            Message::Event {
                kind: EventKind::System,
                ..
            }
        ));
        assert!(matches!(
            driver.next_message().await.unwrap(),
            Message::Event {
                kind: EventKind::Result,
                ..
            }
        ));
        // Only now does the stream's own failure show, after the answer.
        assert!(driver.next_message().await.is_err(), "garbled={garbled}");
        assert_eq!(driver.phase(), Phase::Closed);
        let kept = driver.closed_answer_receipt().unwrap();
        assert!(kept.matches_text("answer") && !kept.is_error() && !kept.interrupted());
    }
}
