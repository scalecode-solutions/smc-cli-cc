/// smc sessions — list conversation sessions with metadata.
use std::io::Write;

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;

use crate::models::Record;
use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

pub struct SessionsOpts {
    pub limit: usize,
    pub project: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
}

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct SessionRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    session_id: String,
    project: String,
    size_bytes: u64,
    size_human: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<String>,
    msg_count: u32,
}

// ── run ────────────────────────────────────────────────────────────────────

/// Returns whether any session was emitted.
pub fn run<W: Write>(opts: &SessionsOpts, files: &[SessionFile], em: &mut Emitter<W>) -> Result<bool> {
    let start = std::time::Instant::now();

    let before = opts.before.clone().map(crate::util::dates::normalize_before);

    let filtered: Vec<&SessionFile> = files
        .iter()
        .filter(|f| {
            if let Some(proj) = &opts.project {
                if !f.project_name.to_lowercase().contains(&proj.to_lowercase()) {
                    return false;
                }
            }
            true
        })
        .collect();

    // Full parallel scan per file: msg_count used to stop early and report
    // "6" for every session; now it's the real message count, and the preview
    // is the first user message that actually has readable text.
    let mut entries: Vec<SessionRecord> = filtered
        .par_iter()
        .filter_map(|file| {
            let Ok(f) = std::fs::File::open(&file.path) else { return None };
            let reader = std::io::BufReader::with_capacity(256 * 1024, f);

            let mut first_timestamp: Option<String> = None;
            let mut last_timestamp: Option<String> = None;
            let mut preview: Option<String> = None;
            let mut msg_count = 0u32;

            use std::io::BufRead;
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(record) = serde_json::from_str::<Record>(&line) else { continue };
                let Some(msg) = record.as_message() else { continue };

                msg_count += 1;
                if let Some(ts) = &msg.timestamp {
                    if first_timestamp.is_none() {
                        first_timestamp = Some(ts.clone());
                    }
                    last_timestamp = Some(ts.clone());
                }
                if preview.is_none() && matches!(record, Record::User(_)) {
                    let text = msg.text_content();
                    let head: String = text.chars().take(120).collect();
                    if !head.trim().is_empty() {
                        preview = Some(head);
                    }
                }
            }

            // Date filters (against the session's first timestamp).
            if opts.after.is_some() || before.is_some() {
                let ts = first_timestamp.as_deref()?;
                if let Some(after) = &opts.after {
                    if ts < after.as_str() {
                        return None;
                    }
                }
                if let Some(before) = &before {
                    if ts > before.as_str() {
                        return None;
                    }
                }
            }

            Some(SessionRecord {
                record_type: "session",
                session_id: file.session_id.clone(),
                project: file.project_name.clone(),
                size_bytes: file.size_bytes,
                size_human: file.size_human(),
                timestamp: first_timestamp,
                last_timestamp,
                preview,
                msg_count,
            })
        })
        .collect();

    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    let show = if opts.limit > 0 {
        std::cmp::min(opts.limit, entries.len())
    } else {
        entries.len()
    };

    for entry in entries.iter().take(show) {
        if !em.emit(entry)? {
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
