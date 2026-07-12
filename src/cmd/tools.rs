/// smc tools — list tool calls in a session.
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

pub struct ToolsOpts {
    pub session: String,
    pub max_tokens: usize,
}

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct ToolRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    role: String,
    tool_name: String,
    input_preview: String,
}

// ── run ────────────────────────────────────────────────────────────────────

pub fn run<W: Write>(_opts: &ToolsOpts, file: &SessionFile, em: &mut Emitter<W>) -> Result<()> {
    let records = crate::cmd::parse_records(file)?;
    let start = std::time::Instant::now();

    let mut count = 0usize;
    'outer: for record in &records {
        let Some(msg) = record.as_message() else { continue };

        let tools = msg.tool_names();
        if tools.is_empty() {
            continue;
        }

        if let crate::models::ContentView::Blocks(blocks) = msg.content_view() {
            for block in blocks {
                if let crate::models::ContentBlock::ToolUse { name, input, .. } = block {
                    let preview: String = input.to_string().chars().take(200).collect();
                    let rec = ToolRecord {
                        record_type: "tool_call",
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
    em.emit(&summary)?;

    em.flush()?;
    Ok(())
}
