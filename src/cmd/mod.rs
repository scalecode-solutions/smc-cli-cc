pub mod search;
pub mod sessions;
pub mod show;
pub mod tools;
pub mod export;
pub mod context;
pub mod stats;
pub mod projects;
pub mod freq;
pub mod recent;

use std::io::BufRead;

use anyhow::Result;

use crate::models::Record;
use crate::util::discover::SessionFile;

/// Parse all records from a session JSONL file, tagged with their 1-based
/// line number so output records can cross-reference `smc context <id> <line>`.
pub fn parse_records(file: &SessionFile) -> Result<Vec<(usize, Record)>> {
    let f = std::fs::File::open(&file.path)?;
    let reader = std::io::BufReader::new(f);
    let mut records = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(record) = serde_json::from_str::<Record>(&line) {
            records.push((line_num + 1, record));
        }
    }

    Ok(records)
}
