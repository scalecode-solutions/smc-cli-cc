/// smc export — export a session as markdown.
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::models::{tool_result_text, ContentBlock, ContentView};
use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

pub struct ExportOpts {
    /// Write markdown to stdout (via emitter raw lines).
    pub to_stdout: bool,
    /// Save markdown to this file path.
    pub md_path: Option<String>,
}

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct ExportDone {
    #[serde(rename = "type")]
    record_type: &'static str,
    session_id: String,
    project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_file: Option<String>,
    messages: usize,
}

// ── run ────────────────────────────────────────────────────────────────────

pub fn run<W: Write>(opts: &ExportOpts, file: &SessionFile, em: &mut Emitter<W>) -> Result<bool> {
    let records = crate::cmd::parse_records(file)?;

    let mut md = String::new();
    md.push_str(&format!(
        "# Session: {}\n\n**Project:** {}  \n**Size:** {}\n\n---\n\n",
        file.session_id, file.project_name, file.size_human()
    ));

    let mut msg_count = 0usize;

    for (_, record) in &records {
        let Some(msg) = record.as_message() else { continue };
        msg_count += 1;

        let role = record.role();
        let ts = msg.timestamp.as_deref().unwrap_or("unknown");
        let ts_short = ts.get(..19).unwrap_or(ts);

        md.push_str(&format!("## {} ({})\n\n", role.to_uppercase(), ts_short));

        match msg.content_view() {
            ContentView::Text(s) => {
                md.push_str(s);
                md.push_str("\n\n");
            }
            ContentView::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            md.push_str(text);
                            md.push_str("\n\n");
                        }
                        ContentBlock::Thinking { thinking } => {
                            md.push_str(&format!(
                                "<details>\n<summary>Thinking</summary>\n\n{}\n\n</details>\n\n",
                                thinking
                            ));
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            let pretty = serde_json::to_string_pretty(input)
                                .unwrap_or_else(|_| input.to_string());
                            md.push_str(&format!("**Tool: {}**\n```json\n{}\n```\n\n", name, pretty));
                        }
                        ContentBlock::ToolResult { content: Some(c), .. } => {
                            let s = tool_result_text(c);
                            let preview: String = s.chars().take(2000).collect();
                            md.push_str(&format!("**Result:**\n```\n{}\n```\n\n", preview));
                        }
                        _ => {}
                    }
                }
            }
            ContentView::None => {}
        }

        md.push_str("---\n\n");
    }

    // write markdown
    if opts.to_stdout {
        // Emit as raw lines so it's readable markdown, not JSON-wrapped
        for line in md.lines() {
            em.raw(line)?;
        }
    }

    let output_file = if let Some(p) = &opts.md_path {
        std::fs::write(p, &md)?;
        Some(p.clone())
    } else if !opts.to_stdout {
        let path = format!("{}.md", &file.session_id[..8.min(file.session_id.len())]);
        std::fs::write(&path, &md)?;
        Some(path)
    } else {
        None
    };

    if !opts.to_stdout {
        let done = ExportDone {
            record_type: "export",
            session_id: file.session_id.clone(),
            project: file.project_name.clone(),
            output_file,
            messages: msg_count,
        };
        em.emit(&done)?;
    }

    em.flush()?;
    Ok(msg_count > 0)
}
