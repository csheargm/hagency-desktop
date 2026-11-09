//! Bounded document text, with PDF parsing in a disposable child process.
//! No document filename becomes a path, argument, URL or shell command.
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_TEXT_BYTES: usize = 128 * 1024;
const MAX_PAGES: usize = 200;
const MAX_RESULT_BYTES: usize = MAX_TEXT_BYTES * 6 + 4096;
const WORKER: &str = "--hagency-extract-document";
static ACTIVE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Text {
    pub text: String,
    pub complete: bool,
    pub pages_read: usize,
    pub total_pages: usize,
}

impl Text {
    fn append(&mut self, text: &str) -> bool {
        let remaining = MAX_TEXT_BYTES.saturating_sub(self.text.len());
        let mut end = text.len().min(remaining);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&text[..end]);
        if end < text.len() {
            self.complete = false;
        }
        end == text.len()
    }
    pub(crate) fn plain(text: &str) -> Self {
        let mut result = Self {
            text: String::new(),
            complete: true,
            pages_read: 1,
            total_pages: 1,
        };
        result.append(text);
        result
    }
}

pub(crate) async fn pdf(bytes: Vec<u8>) -> Result<Text, &'static str> {
    if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
        return Err("file_size_limit");
    }
    let _permit = ACTIVE.try_acquire().map_err(|_| "document_reader_busy")?;
    let executable = std::env::current_exe().map_err(|_| "document_reader_unavailable")?;
    pdf_with(bytes, executable, Duration::from_secs(20)).await
}

async fn pdf_with(
    bytes: Vec<u8>,
    executable: std::path::PathBuf,
    timeout: Duration,
) -> Result<Text, &'static str> {
    let mut child = tokio::process::Command::new(executable)
        .arg(WORKER)
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "document_reader_unavailable")?;
    let mut input = child.stdin.take().ok_or("document_reader_unavailable")?;
    let output = child.stdout.take().ok_or("document_reader_unavailable")?;
    let operation = async {
        let send = async move {
            input.write_all(&bytes).await?;
            drop(input); // EOF is the parser's framing boundary, including Unix pipes.
            Ok::<_, std::io::Error>(())
        };
        let receive = async {
            let mut out = Vec::new();
            output
                .take((MAX_RESULT_BYTES + 1) as u64)
                .read_to_end(&mut out)
                .await?;
            Ok::<_, std::io::Error>(out)
        };
        let (_, output) = tokio::try_join!(send, receive).map_err(|_| "document_reader_failed")?;
        if output.len() > MAX_RESULT_BYTES {
            return Err("document_text_limit");
        }
        let status = child.wait().await.map_err(|_| "document_reader_failed")?;
        if !status.success() {
            return Err("pdf_unreadable_or_protected");
        }
        let text: Text = serde_json::from_slice(&output).map_err(|_| "document_reader_failed")?;
        if text.text.len() > MAX_TEXT_BYTES
            || text.pages_read > MAX_PAGES
            || text.pages_read > text.total_pages
        {
            return Err("document_reader_failed");
        }
        if text.text.trim().is_empty() {
            return Err("pdf_has_no_extractable_text");
        }
        Ok(text)
    };
    // Dropping this future (including turn cancellation) kills the parser.
    tokio::time::timeout(timeout, operation)
        .await
        .map_err(|_| "document_reader_timeout")?
}

pub(crate) fn worker_if_requested(args: &[OsString]) -> Option<i32> {
    if args.get(1).is_none_or(|arg| arg != WORKER) {
        return None;
    }
    Some(if args.len() != 2 {
        1
    } else {
        match worker() {
            Ok(()) => 0,
            Err(reason) => {
                eprintln!("{reason}");
                1
            }
        }
    })
}

fn worker() -> Result<(), &'static str> {
    use std::io::{Read, Write};
    // Only this disposable parser changes its own limits; never the daemon.
    #[cfg(unix)]
    {
        use rustix::process::{Resource, Rlimit, setrlimit};
        setrlimit(
            Resource::Cpu,
            Rlimit {
                current: Some(15),
                maximum: Some(15),
            },
        )
        .map_err(|_| "parser_cpu_limit_unavailable")?;
        // macOS rejects this RLIMIT_DATA setting. Its parser is still killed
        // at the wall deadline; Linux additionally limits its data segment.
        #[cfg(target_os = "linux")]
        setrlimit(
            Resource::Data,
            Rlimit {
                current: Some(512 * 1024 * 1024),
                maximum: Some(512 * 1024 * 1024),
            },
        )
        .map_err(|_| "parser_memory_limit_unavailable")?;
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "parser_input_failed")?;
    if bytes.len() > MAX_FILE_BYTES || !bytes.starts_with(b"%PDF-") {
        return Err("invalid_pdf_input");
    }
    let result = extract(&bytes).map_err(|_| "pdf_unreadable_or_protected")?;
    let output = serde_json::to_vec(&result).map_err(|_| "parser_output_failed")?;
    if output.len() > MAX_RESULT_BYTES {
        return Err("parser_output_limit");
    }
    std::io::stdout()
        .write_all(&output)
        .map_err(|_| "parser_output_failed")
}

// PDFKit handles Chinese ToUnicode fonts and the system's PDF encodings.
#[cfg(target_os = "macos")]
fn extract(bytes: &[u8]) -> Result<Text, ()> {
    let mut result = Text {
        text: String::new(),
        complete: true,
        pages_read: 0,
        total_pages: 0,
    };
    let mut has_text = false;
    result.total_pages = hagency_platform::pdf_text::visit(bytes, MAX_PAGES, |page, text| {
        has_text |= !text.trim().is_empty();
        result.pages_read += 1;
        result.append(&format!("\n[Page {page}]\n{text}"))
    })
    .map_err(|_| ())?;
    result.complete &= result.pages_read == result.total_pages;
    if !has_text {
        result.text.clear();
    }
    Ok(result)
}

#[cfg(not(target_os = "macos"))]
fn extract(bytes: &[u8]) -> Result<Text, ()> {
    let document = lopdf::Document::load_mem(bytes).map_err(|_| ())?;
    if document.is_encrypted() {
        return Err(());
    }
    let pages = document.get_pages();
    let mut result = Text {
        text: String::new(),
        complete: pages.len() <= MAX_PAGES,
        pages_read: 0,
        total_pages: pages.len(),
    };
    let mut has_text = false;
    for page in pages.keys().take(MAX_PAGES) {
        let text = document.extract_text(&[*page]).map_err(|_| ())?;
        has_text |= !text.trim().is_empty();
        result.pages_read += 1;
        if !result.append(&format!("\n[Page {page}]\n{text}")) {
            break;
        }
    }
    if !has_text {
        result.text.clear();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_text_limit_preserves_unicode_and_reports_incomplete_coverage() {
        let text = Text::plain(&"中文🙂".repeat(MAX_TEXT_BYTES));
        assert!(text.text.len() <= MAX_TEXT_BYTES);
        assert!(!text.complete);
        assert!(text.text.starts_with("中文🙂"));
        assert!(Text::plain("complete document").complete);
    }
    #[test]
    fn document_worker_never_accepts_paths_or_extra_arguments() {
        assert!(worker_if_requested(&["app".into()]).is_none());
        assert_eq!(
            worker_if_requested(&["app".into(), WORKER.into(), "/private/file.pdf".into()]),
            Some(1)
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn document_parser_closes_input_and_receives_bounded_text() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("parser");
        let expected = Text::plain("document body 中文");
        let output = serde_json::to_string(&expected).unwrap();
        std::fs::write(&executable, format!("#!/bin/sh\n/bin/cat >/dev/null\nprintf '%s' '{output}'\n")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = pdf_with(vec![b'x'; 128 * 1024], executable, Duration::from_secs(5)).await.unwrap();
        assert_eq!(result.text, expected.text);
        assert!(result.complete);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn document_parser_timeout_kills_the_child() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("parser");
        let pid_file = root.path().join("pid");
        let quoted_pid_file = pid_file.to_string_lossy().replace('\'', "'\\''");
        std::fs::write(
            &executable,
            format!("#!/bin/sh\necho $$ > '{quoted_pid_file}'\nexec /bin/sleep 120\n"),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Start the real child before advancing the Tokio deadline. On a busy
        // build host it can otherwise be killed before the shell writes its
        // PID, making a correct timeout fail at read_to_string below.
        let operation = pdf_with(b"%PDF-fixture".to_vec(), executable, Duration::from_secs(30));
        let observe_start = async {
            let until = std::time::Instant::now() + Duration::from_secs(10);
            let pid = loop {
                if let Some(pid) = std::fs::read_to_string(&pid_file)
                    .ok()
                    .and_then(|text| text.trim().parse::<i32>().ok())
                    .and_then(rustix::process::Pid::from_raw)
                {
                    break pid;
                }
                assert!(std::time::Instant::now() < until, "document parser did not start");
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(31)).await;
            pid
        };
        let (result, pid) = tokio::join!(operation, observe_start);
        tokio::time::resume();
        assert!(matches!(result, Err("document_reader_timeout")));
        for _ in 0..100 {
            if rustix::process::test_kill_process(pid).is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed-out document parser remained alive");
    }
}
