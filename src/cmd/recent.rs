/// smc recent — show most recent messages across all sessions.
use std::collections::VecDeque;
use std::io::Write;

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;

use crate::models::Record;
use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

pub struct RecentOpts {
    pub limit: usize,
    pub role: Option<String>,
    pub project: Option<String>,
}

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct RecentRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    project: String,
    session_id: String,
    /// 1-based JSONL line number — feed to `smc context <session> <line>`.
    line: usize,
    role: String,
    timestamp: String,
    text: String,
}

// ── run ────────────────────────────────────────────────────────────────────

/// Returns whether any message was emitted.
pub fn run<W: Write>(opts: &RecentOpts, files: &[SessionFile], em: &mut Emitter<W>) -> Result<bool> {
    let start = std::time::Instant::now();

    let filtered: Vec<&SessionFile> = files
        .iter()
        .filter(|f| {
            if let Some(proj) = &opts.project {
                f.project_name.to_lowercase().contains(&proj.to_lowercase())
            } else {
                true
            }
        })
        .collect();

    // Per file: rolling window of the last `limit` messages that pass the
    // filters. Filtering BEFORE the window matters — the old code kept the
    // last N raw lines and filtered afterwards, so `--role user` under-filled
    // whenever assistant/tool records dominated the tail.
    let per_file: Vec<Vec<RecentRecord>> = filtered
        .par_iter()
        .map(|file| {
            let mut buf: VecDeque<RecentRecord> = VecDeque::new();
            let Ok(f) = std::fs::File::open(&file.path) else { return Vec::new() };

            use std::io::BufRead;
            let reader = std::io::BufReader::with_capacity(256 * 1024, f);
            for (line_num, line) in reader.lines().enumerate() {
                let Ok(line) = line else { continue };
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(record) = serde_json::from_str::<Record>(&line) else { continue };
                let Some(msg) = record.as_message() else { continue };

                let role = record.role();
                if let Some(rf) = &opts.role {
                    if role != rf.as_str() {
                        continue;
                    }
                }

                let text = msg.text_content();
                let preview: String =
                    text.chars().take(120).collect::<String>().replace('\n', " ");
                if preview.trim().is_empty() {
                    continue; // tool-result-only records carry no readable text
                }

                buf.push_back(RecentRecord {
                    record_type: "recent",
                    project: file.project_name.clone(),
                    session_id: file.session_id.clone(),
                    line: line_num + 1,
                    role: role.to_string(),
                    timestamp: msg.timestamp.clone().unwrap_or_default(),
                    text: preview,
                });
                if buf.len() > opts.limit {
                    buf.pop_front();
                }
            }
            buf.into_iter().collect()
        })
        .collect();

    let mut all: Vec<RecentRecord> = per_file.into_iter().flatten().collect();
    all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    let show = std::cmp::min(opts.limit, all.len());
    for rec in all.iter().take(show) {
        if !em.emit(rec)? {
            break;
        }
    }

    let summary = crate::output::SummaryRecord {
        record_type: "summary",
        count: show,
        files_scanned: Some(filtered.len()),
        elapsed_ms: start.elapsed().as_millis(),
    };
    // Always emitted — this is the record that signals truncation.
    em.emit_always(&summary)?;

    em.flush()?;
    Ok(show > 0)
}
