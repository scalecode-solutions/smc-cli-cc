/// smc context — show messages around a specific line in a session.
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::models::Record;
use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

pub struct ContextOpts {
    pub line: usize,
    pub context: usize,
}

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct ContextRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    line: usize,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    text: String,
    is_target: bool,
}

// ── run ────────────────────────────────────────────────────────────────────

/// Returns whether any context message was emitted.
pub fn run<W: Write>(opts: &ContextOpts, file: &SessionFile, em: &mut Emitter<W>) -> Result<bool> {
    let f = std::fs::File::open(&file.path)?;
    let reader = std::io::BufReader::new(f);

    use std::io::BufRead;
    let mut messages: Vec<(usize, Record)> = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Record>(&line) else { continue };
        if record.is_message() {
            messages.push((line_num + 1, record));
        }
    }

    let target_idx = messages
        .iter()
        .position(|(ln, _)| *ln >= opts.line)
        .unwrap_or(messages.len().saturating_sub(1));

    let start = target_idx.saturating_sub(opts.context);
    let end = std::cmp::min(messages.len(), target_idx + opts.context + 1);

    let mut emitted = false;
    for (i, (line_num, record)) in messages[start..end].iter().enumerate() {
        let msg = record.as_message().unwrap();
        let text = msg.text_content();
        let preview: String = text.chars().take(500).collect();

        let rec = ContextRecord {
            record_type: "context",
            line: *line_num,
            role: record.role().to_string(),
            timestamp: msg.timestamp.clone(),
            text: preview,
            is_target: start + i == target_idx,
        };

        if !em.emit(&rec)? {
            break;
        }
        emitted = true;
    }

    em.flush()?;
    Ok(emitted)
}
