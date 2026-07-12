/// smc projects — list projects with session counts, sizes, and date ranges.
use std::collections::HashMap;
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::models;
use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct ProjectRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    name: String,
    sessions: usize,
    size_bytes: u64,
    size_human: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    earliest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
}

// ── run ────────────────────────────────────────────────────────────────────

pub fn run<W: Write>(files: &[SessionFile], em: &mut Emitter<W>) -> Result<bool> {
    let start = std::time::Instant::now();
    struct Info {
        sessions: usize,
        total_size: u64,
        earliest: Option<String>,
        latest: Option<String>,
    }

    let mut projects: HashMap<String, Info> = HashMap::new();

    for file in files {
        let entry = projects.entry(file.project_name.clone()).or_insert(Info {
            sessions: 0,
            total_size: 0,
            earliest: None,
            latest: None,
        });
        entry.sessions += 1;
        entry.total_size += file.size_bytes;

        if let Ok(f) = std::fs::File::open(&file.path) {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(f);
            for line in reader.lines().take(5) {
                let Ok(line) = line else { continue };
                if let Ok(record) = serde_json::from_str::<models::Record>(&line) {
                    if let Some(msg) = record.as_message() {
                        if let Some(ts) = &msg.timestamp {
                            let ts_date = ts.get(..10).unwrap_or(ts);
                            if entry.earliest.as_deref().map_or(true, |e| ts_date < e) {
                                entry.earliest = Some(ts_date.to_string());
                            }
                            if entry.latest.as_deref().map_or(true, |l| ts_date > l) {
                                entry.latest = Some(ts_date.to_string());
                            }
                            break;
                        }
                    }
                }
            }
        }
    }

    let mut sorted: Vec<_> = projects.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.1.latest
            .as_deref()
            .unwrap_or("")
            .cmp(a.1.latest.as_deref().unwrap_or(""))
    });

    for (name, info) in &sorted {
        let rec = ProjectRecord {
            record_type: "project",
            name: name.clone(),
            sessions: info.sessions,
            size_bytes: info.total_size,
            size_human: crate::cmd::stats::format_bytes(info.total_size),
            earliest: info.earliest.clone(),
            latest: info.latest.clone(),
        };
        if !em.emit(&rec)? {
            break;
        }
    }

    let summary = crate::output::SummaryRecord {
        record_type: "summary",
        count: sorted.len(),
        files_scanned: Some(files.len()),
        elapsed_ms: start.elapsed().as_millis(),
    };
    // Always emitted — this is the record that signals truncation.
    em.emit_always(&summary)?;

    em.flush()?;
    Ok(!sorted.is_empty())
}
