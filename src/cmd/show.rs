/// smc show — pretty-print a conversation as JSONL message records.
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::models::{ContentBlock, ContentView, Record};
use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

pub struct ShowOpts {
    pub session: String,
    pub thinking: bool,
    pub from: Option<usize>,
    pub to: Option<usize>,
    pub max_tokens: usize,
}

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct MessageOut {
    #[serde(rename = "type")]
    record_type: &'static str,
    index: usize,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ToolCallOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
}

#[derive(Serialize, Debug)]
struct ToolCallOut {
    name: String,
    input_preview: String,
}

// ── run ────────────────────────────────────────────────────────────────────

pub fn run<W: Write>(opts: &ShowOpts, file: &SessionFile, em: &mut Emitter<W>) -> Result<()> {
    let records = crate::cmd::parse_records(file)?;

    let mut index = 0usize;
    for record in &records {
        if !record.is_message() {
            continue;
        }

        let in_range = match (opts.from, opts.to) {
            (Some(f), Some(t)) => index >= f && index <= t,
            (Some(f), None) => index >= f,
            (None, Some(t)) => index <= t,
            (None, None) => true,
        };

        if in_range {
            let msg = record.as_message().unwrap();
            let out = build_message_out(record, msg, index, opts.thinking);
            if !em.emit(&out)? {
                break;
            }
        }

        index += 1;

        if let Some(t) = opts.to {
            if index > t {
                break;
            }
        }
    }

    em.flush()?;
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn build_message_out(
    record: &Record,
    msg: &crate::models::MessageRecord,
    index: usize,
    include_thinking: bool,
) -> MessageOut {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut thinking_text = None;

    match msg.content_view() {
        ContentView::Text(s) => text_parts.push(s.to_string()),
        ContentView::Blocks(blocks) => {
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => text_parts.push(text.clone()),
                    ContentBlock::Thinking { thinking } => {
                        if include_thinking {
                            thinking_text = Some(thinking.clone());
                        }
                    }
                    ContentBlock::ToolUse { name, input, .. } => {
                        let preview: String = input.to_string().chars().take(200).collect();
                        tool_calls.push(ToolCallOut {
                            name: name.clone(),
                            input_preview: preview,
                        });
                    }
                    _ => {}
                }
            }
        }
        ContentView::None => {}
    }

    MessageOut {
        record_type: "message",
        index,
        role: record.role().to_string(),
        timestamp: msg.timestamp.clone(),
        text: text_parts.join("\n"),
        tool_calls,
        thinking: thinking_text,
    }
}
