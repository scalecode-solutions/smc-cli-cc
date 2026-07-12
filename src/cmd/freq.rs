/// smc freq — frequency analysis across all conversation logs.
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;

use crate::models;
use crate::output::Emitter;
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

pub struct FreqOpts {
    pub mode: FreqMode,
    pub limit: usize,
    pub raw: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreqMode {
    Chars,
    Words,
    Tools,
    Roles,
}

impl FreqMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "chars" | "c" => Ok(Self::Chars),
            "words" | "w" => Ok(Self::Words),
            "tools" | "t" => Ok(Self::Tools),
            "roles" | "r" => Ok(Self::Roles),
            _ => anyhow::bail!("unknown freq mode '{}' — use: chars, words, tools, roles", s),
        }
    }
}

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct CharFreqRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    letter: char,
    count: u64,
    pct: f64,
}

#[derive(Serialize, Debug)]
struct FreqRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    key: String,
    count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pct: Option<f64>,
}

#[derive(Serialize, Debug)]
struct FreqSummary {
    #[serde(rename = "type")]
    record_type: &'static str,
    mode: String,
    total: u64,
    files_scanned: usize,
    elapsed_ms: u128,
}

// ── run ────────────────────────────────────────────────────────────────────

/// Returns whether anything was counted.
pub fn run<W: Write>(opts: &FreqOpts, files: &[SessionFile], em: &mut Emitter<W>) -> Result<bool> {
    let start = std::time::Instant::now();

    let total = match opts.mode {
        FreqMode::Chars if opts.raw => run_chars_raw(files, em)?,
        FreqMode::Chars => run_chars_parsed(files, em)?,
        FreqMode::Words => run_words(files, opts.limit, em)?,
        FreqMode::Tools => run_tools(files, opts.limit, em)?,
        FreqMode::Roles => run_roles(files, em)?,
    };

    let summary = FreqSummary {
        record_type: "summary",
        mode: format!("{:?}", opts.mode).to_lowercase(),
        total,
        files_scanned: files.len(),
        elapsed_ms: start.elapsed().as_millis(),
    };
    // Always emitted — this is the record that signals truncation.
    em.emit_always(&summary)?;

    em.flush()?;
    Ok(total > 0)
}

// ── Chars (parsed) ─────────────────────────────────────────────────────────

fn run_chars_parsed<W: Write>(files: &[SessionFile], em: &mut Emitter<W>) -> Result<u64> {
    let counts: Vec<AtomicU64> = (0..26).map(|_| AtomicU64::new(0)).collect();

    files.par_iter().for_each(|file| {
        if let Ok(f) = std::fs::File::open(&file.path) {
            use std::io::BufRead;
            let reader = std::io::BufReader::with_capacity(256 * 1024, f);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                let Ok(record) = serde_json::from_str::<models::Record>(&line) else { continue };
                let Some(msg) = record.as_message() else { continue };
                let text = msg.text_content();
                for b in text.bytes() {
                    let idx = match b {
                        b'a'..=b'z' => (b - b'a') as usize,
                        b'A'..=b'Z' => (b - b'A') as usize,
                        _ => continue,
                    };
                    counts[idx].fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });

    emit_char_counts(&counts, em)
}

// ── Chars (raw) ────────────────────────────────────────────────────────────

fn run_chars_raw<W: Write>(files: &[SessionFile], em: &mut Emitter<W>) -> Result<u64> {
    let counts: Vec<AtomicU64> = (0..26).map(|_| AtomicU64::new(0)).collect();

    files.par_iter().for_each(|file| {
        if let Ok(data) = std::fs::read(&file.path) {
            for &b in &data {
                let idx = match b {
                    b'a'..=b'z' => (b - b'a') as usize,
                    b'A'..=b'Z' => (b - b'A') as usize,
                    _ => continue,
                };
                counts[idx].fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    emit_char_counts(&counts, em)
}

fn emit_char_counts<W: Write>(counts: &[AtomicU64], em: &mut Emitter<W>) -> Result<u64> {
    let totals: Vec<u64> = counts.iter().map(|c| c.load(Ordering::Relaxed)).collect();
    let grand_total: u64 = totals.iter().sum();

    for (i, &count) in totals.iter().enumerate() {
        let letter = (b'a' + i as u8) as char;
        let pct = if grand_total > 0 { count as f64 / grand_total as f64 * 100.0 } else { 0.0 };
        let rec = CharFreqRecord {
            record_type: "char_freq",
            letter,
            count,
            pct,
        };
        if !em.emit(&rec)? {
            break;
        }
    }

    Ok(grand_total)
}

// ── Words ──────────────────────────────────────────────────────────────────

fn run_words<W: Write>(files: &[SessionFile], limit: usize, em: &mut Emitter<W>) -> Result<u64> {
    let word_counts: Mutex<HashMap<String, u64>> = Mutex::new(HashMap::new());

    files.par_iter().for_each(|file| {
        let mut local: HashMap<String, u64> = HashMap::new();
        if let Ok(f) = std::fs::File::open(&file.path) {
            use std::io::BufRead;
            let reader = std::io::BufReader::with_capacity(256 * 1024, f);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                let Ok(record) = serde_json::from_str::<models::Record>(&line) else { continue };
                let Some(msg) = record.as_message() else { continue };
                let text = msg.text_content();
                for word in text.split(|c: char| !c.is_alphanumeric()) {
                    if word.len() >= 3 {
                        *local.entry(word.to_lowercase()).or_default() += 1;
                    }
                }
            }
        }
        let mut global = word_counts.lock().unwrap();
        for (word, count) in local {
            *global.entry(word).or_default() += count;
        }
    });

    let counts = word_counts.into_inner().unwrap();
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

    let grand_total: u64 = sorted.iter().map(|(_, c)| c).sum();

    for (word, count) in sorted.iter().take(limit) {
        let pct = if grand_total > 0 { *count as f64 / grand_total as f64 * 100.0 } else { 0.0 };
        let rec = FreqRecord {
            record_type: "word_freq",
            key: word.clone(),
            count: *count,
            pct: Some(pct),
        };
        if !em.emit(&rec)? {
            break;
        }
    }

    Ok(grand_total)
}

// ── Tools ──────────────────────────────────────────────────────────────────

fn run_tools<W: Write>(files: &[SessionFile], limit: usize, em: &mut Emitter<W>) -> Result<u64> {
    let tool_counts: Mutex<HashMap<String, u64>> = Mutex::new(HashMap::new());

    files.par_iter().for_each(|file| {
        let mut local: HashMap<String, u64> = HashMap::new();
        if let Ok(f) = std::fs::File::open(&file.path) {
            use std::io::BufRead;
            let reader = std::io::BufReader::with_capacity(256 * 1024, f);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                let Ok(record) = serde_json::from_str::<models::Record>(&line) else { continue };
                let Some(msg) = record.as_message() else { continue };
                for tool in msg.tool_names() {
                    *local.entry(tool.to_string()).or_default() += 1;
                }
            }
        }
        let mut global = tool_counts.lock().unwrap();
        for (tool, count) in local {
            *global.entry(tool).or_default() += count;
        }
    });

    let counts = tool_counts.into_inner().unwrap();
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

    let grand_total: u64 = sorted.iter().map(|(_, c)| c).sum();

    for (tool, count) in sorted.iter().take(limit) {
        let pct = if grand_total > 0 { *count as f64 / grand_total as f64 * 100.0 } else { 0.0 };
        let rec = FreqRecord {
            record_type: "tool_freq",
            key: tool.clone(),
            count: *count,
            pct: Some(pct),
        };
        if !em.emit(&rec)? {
            break;
        }
    }

    Ok(grand_total)
}

// ── Roles ──────────────────────────────────────────────────────────────────

fn run_roles<W: Write>(files: &[SessionFile], em: &mut Emitter<W>) -> Result<u64> {
    let role_counts: Mutex<HashMap<String, u64>> = Mutex::new(HashMap::new());

    files.par_iter().for_each(|file| {
        let mut local: HashMap<String, u64> = HashMap::new();
        if let Ok(f) = std::fs::File::open(&file.path) {
            use std::io::BufRead;
            let reader = std::io::BufReader::with_capacity(256 * 1024, f);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                let Ok(record) = serde_json::from_str::<models::Record>(&line) else { continue };
                if record.is_message() {
                    *local.entry(record.role().to_string()).or_default() += 1;
                }
            }
        }
        let mut global = role_counts.lock().unwrap();
        for (role, count) in local {
            *global.entry(role).or_default() += count;
        }
    });

    let counts = role_counts.into_inner().unwrap();
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

    let grand_total: u64 = sorted.iter().map(|(_, c)| c).sum();

    for (role, count) in &sorted {
        let pct = if grand_total > 0 { *count as f64 / grand_total as f64 * 100.0 } else { 0.0 };
        let rec = FreqRecord {
            record_type: "role_freq",
            key: role.clone(),
            count: *count,
            pct: Some(pct),
        };
        if !em.emit(&rec)? {
            break;
        }
    }

    Ok(grand_total)
}
