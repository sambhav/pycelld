//! Invocation-owned output collection, including interpreter error paths.
use celld_runtime::observability::{Diagnostic, Observer};
use monty_types::{MontyException, PrintWriterCallback};
use serde_json::json;
use std::{borrow::Cow, sync::Arc};

pub(crate) const EVENT_BYTES: usize = 8 * 1024;
pub(crate) const INVOCATION_BYTES: usize = 64 * 1024;
const MAX_EVENTS: usize = 256;

#[derive(Clone, Default)]
pub(crate) struct SourceMap(pub Vec<(usize, String, usize, String)>);
impl SourceMap {
    pub fn traceback(&self, error: &MontyException) -> String {
        let mut text = String::from("Traceback (most recent call last):\n");
        for frame in error
            .traceback()
            .iter()
            .rev()
            .take(32)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            if let Some((_, file, line, source)) = self
                .0
                .iter()
                .find(|(line, _, _, _)| *line == frame.start.line as usize)
            {
                use std::fmt::Write;
                let _ = writeln!(
                    text,
                    "  File {file:?}, line {line}, in {}\n    {}",
                    frame.frame_name.as_deref().unwrap_or("<module>"),
                    cap(source.trim(), 512)
                );
            }
        }
        text.push_str(&cap(&error.summary(), 2048));
        cap(&text, EVENT_BYTES - 256)
    }
}

pub(crate) struct Output {
    observer: Option<Arc<dyn Observer>>,
    line: String,
    bytes: usize,
    events: usize,
    truncated: bool,
    pub sources: Arc<SourceMap>,
}
impl Output {
    pub fn new(observer: Option<Arc<dyn Observer>>, sources: Arc<SourceMap>) -> Self {
        Self {
            observer,
            line: String::new(),
            bytes: 0,
            events: 0,
            truncated: false,
            sources,
        }
    }
    pub fn emit(&mut self, event: Diagnostic) {
        if self.truncated {
            return;
        }
        let size = match &event {
            Diagnostic::Log {
                message, fields, ..
            } => message.len() + fields.to_string().len(),
            Diagnostic::Span { name, fields, .. } => name.len() + fields.to_string().len(),
        };
        if size > EVENT_BYTES
            || self.bytes.saturating_add(size) > INVOCATION_BYTES
            || self.events >= MAX_EVENTS
        {
            if !self.truncated {
                self.truncated = true;
                if let Some(observer) = &self.observer {
                    observer.emit(Diagnostic::Log {
                        level: "warn".into(),
                        message: "Python diagnostics truncated".into(),
                        fields: json!({"source":"runtime","truncated":true}),
                    });
                }
            }
            return;
        }
        self.bytes += size;
        self.events += 1;
        if let Some(observer) = &self.observer {
            observer.emit(event);
        }
    }
    pub fn flush(&mut self) {
        if !self.line.is_empty() {
            let message = std::mem::take(&mut self.line);
            self.emit(Diagnostic::Log {
                level: "info".into(),
                message,
                fields: json!({"source":"stdout"}),
            });
        }
    }
    pub fn failure(&mut self, error: MontyException) -> crate::Failure {
        self.flush();
        let traceback = self.sources.traceback(&error);
        // Reserve error reporting independently from user-controlled output volume.
        if let Some(observer) = &self.observer {
            observer.emit(Diagnostic::Log {
                level: "error".into(),
                message: traceback,
                fields: json!({"source":"exception","exception.type":error.exc_type().to_string()}),
            });
        }
        crate::Failure::python(error)
    }
    fn write(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        for ch in text.chars() {
            if ch == '\n' {
                // Preserve empty print lines as events too.
                if self.line.is_empty() {
                    self.line.push('\n');
                }
                self.flush();
            } else {
                if self.line.len() + ch.len_utf8() > EVENT_BYTES - 64 {
                    self.flush();
                }
                self.line.push(ch);
            }
            if self.truncated {
                break;
            }
        }
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        self.flush();
    }
}
impl PrintWriterCallback for Output {
    fn stdout_write(&mut self, output: Cow<'_, str>) -> Result<(), MontyException> {
        self.write(&output);
        Ok(())
    }
    fn stdout_push(&mut self, ch: char) -> Result<(), MontyException> {
        self.write(ch.encode_utf8(&mut [0; 4]));
        Ok(())
    }
}
pub(crate) fn cap(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
