//! Small process-wide log buffer shared by the server thread and the GUI.

use std::sync::{Mutex, OnceLock};

#[derive(Default)]
pub struct LogBuffer {
    lines: Mutex<Vec<String>>,
}

static LOG: OnceLock<LogBuffer> = OnceLock::new();

fn buffer() -> &'static LogBuffer {
    LOG.get_or_init(LogBuffer::default)
}

pub fn log(text: impl AsRef<str>) {
    let text = text.as_ref().to_string();
    println!("{text}");
    let mut lines = buffer().lines.lock().unwrap();
    lines.push(text);
    if lines.len() > 4000 {
        lines.drain(0..1500);
    }
}

pub fn lines_from(start: usize) -> (Vec<String>, usize) {
    let lines = buffer().lines.lock().unwrap();
    if start >= lines.len() {
        return (Vec::new(), lines.len());
    }
    (lines[start..].to_vec(), lines.len())
}
