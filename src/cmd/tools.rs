/// smc tools — list tool calls in a session.
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct ToolRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    /// 1-based JSONL line number — feed to `smc context <session> <line>`.
    line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    role: String,
    tool_name: String,
    input_preview: String,
}

// ── run ────────────────────────────────────────────────────────────────────

/// Returns whether any tool call was emitted.
pub fn run<W: Write>(file: &SessionFile, em: &mut Emitter<W>) -> Result<bool> {
    let records = crate::cmd::parse_records(file)?;
    let start = std::time::Instant::now();

    let mut count = 0usize;
    'outer: for (line, record) in &records {
        let Some(msg) = record.as_message() else { continue };

        if let crate::models::ContentView::Blocks(blocks) = msg.content_view() {
            for block in blocks {
                if let crate::models::ContentBlock::ToolUse { name, input, .. } = block {
                    let preview: String = input.to_string().chars().take(200).collect();
                    let rec = ToolRecord {
                        record_type: "tool_call",
                        line: *line,
                        uuid: msg.uuid.clone(),
                        timestamp: msg.timestamp.clone(),
                        role: record.role().to_string(),
                        tool_name: name.clone(),
                        input_preview: preview,
                    };
                    if !em.emit(&rec)? {
                        break 'outer;
                    }
                    count += 1;
                }
            }
        }
    }

    let summary = crate::output::SummaryRecord {
        record_type: "summary",
        count,
        files_scanned: None,
        elapsed_ms: start.elapsed().as_millis(),
    };
    // Always emitted — this is the record that signals truncation.
    em.emit_always(&summary)?;

    em.flush()?;
    Ok(count > 0)
}
