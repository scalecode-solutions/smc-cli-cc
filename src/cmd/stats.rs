/// smc stats — aggregate statistics across all conversation logs.
use std::collections::HashMap;
use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct StatsRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    total_sessions: usize,
    total_size_bytes: u64,
    total_size_human: String,
    project_count: usize,
    projects: Vec<ProjectStat>,
}

#[derive(Serialize, Debug)]
struct ProjectStat {
    name: String,
    sessions: usize,
    size_bytes: u64,
    size_human: String,
}

// ── run ────────────────────────────────────────────────────────────────────

pub fn run<W: Write>(files: &[SessionFile], em: &mut Emitter<W>) -> Result<bool> {
    let total_size: u64 = files.iter().map(|f| f.size_bytes).sum();

    let mut projects: HashMap<String, (usize, u64)> = HashMap::new();
    for f in files {
        let entry = projects.entry(f.project_name.clone()).or_default();
        entry.0 += 1;
        entry.1 += f.size_bytes;
    }

    let mut sorted: Vec<_> = projects.into_iter().collect();
    sorted.sort_by_key(|(_, (_, size))| std::cmp::Reverse(*size));

    let project_stats: Vec<ProjectStat> = sorted
        .iter()
        .take(15)
        .map(|(name, (count, size))| ProjectStat {
            name: name.clone(),
            sessions: *count,
            size_bytes: *size,
            size_human: format_bytes(*size),
        })
        .collect();

    let rec = StatsRecord {
        record_type: "stats",
        total_sessions: files.len(),
        total_size_bytes: total_size,
        total_size_human: format_bytes(total_size),
        project_count: sorted.len(),
        projects: project_stats,
    };

    em.emit(&rec)?;
    em.flush()?;
    Ok(!files.is_empty())
}

// ── Helpers ────────────────────────────────────────────────────────────────

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2}GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
