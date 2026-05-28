/// smc search — parallel full-text search across Claude Code conversation logs.
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use rayon::prelude::*;
use regex::Regex;
use serde::Serialize;

use crate::models::Record;
use crate::output::{Emitter, SMC_TAG};
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

pub struct SearchOpts {
    pub queries: Vec<String>,
    pub is_regex: bool,
    pub and_mode: bool,
    pub role: Option<String>,
    pub tool: Option<String>,
    pub project: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub branch: Option<String>,
    pub file: Option<String>,
    pub tool_input: bool,
    pub thinking_only: bool,
    pub no_thinking: bool,
    pub max_results: usize,
    pub include_smc: bool,
    pub exclude_session: Option<String>,
    /// Hard cap on output tokens (0 = unlimited).
    pub max_tokens: usize,
}

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct SearchRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    project: String,
    session_id: String,
    line: usize,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    matched_query: String,
    text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_branch: Option<String>,
}

#[derive(Serialize, Debug)]
struct SearchSummary {
    #[serde(rename = "type")]
    record_type: &'static str,
    query: String,
    count: usize,
    files_scanned: usize,
    elapsed_ms: u128,
}

// ── Matcher ────────────────────────────────────────────────────────────────

struct Matcher {
    regexes: Vec<Regex>,
    plains: Vec<String>,
    and_mode: bool,
}

impl Matcher {
    fn new(queries: &[String], is_regex: bool, and_mode: bool) -> Result<Self> {
        if is_regex {
            let regexes = queries
                .iter()
                .map(|q| Regex::new(q))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(Self { regexes, plains: vec![], and_mode })
        } else {
            Ok(Self {
                regexes: vec![],
                plains: queries.iter().map(|q| q.to_lowercase()).collect(),
                and_mode,
            })
        }
    }

    fn first_match(&self, text: &str) -> Option<String> {
        if self.and_mode {
            return self.all_match(text);
        }
        if !self.regexes.is_empty() {
            for re in &self.regexes {
                if let Some(m) = re.find(text) {
                    return Some(m.as_str().to_string());
                }
            }
        } else {
            let lower = text.to_lowercase();
            for q in &self.plains {
                if lower.contains(q.as_str()) {
                    return Some(q.clone());
                }
            }
        }
        None
    }

    fn all_match(&self, text: &str) -> Option<String> {
        if !self.regexes.is_empty() {
            let mut hits = Vec::new();
            for re in &self.regexes {
                match re.find(text) {
                    Some(m) => hits.push(m.as_str().to_string()),
                    None => return None,
                }
            }
            Some(hits.join(" + "))
        } else {
            let lower = text.to_lowercase();
            for q in &self.plains {
                if !lower.contains(q.as_str()) {
                    return None;
                }
            }
            Some(self.plains.join(" + "))
        }
    }
}

// ── run ────────────────────────────────────────────────────────────────────

pub fn run<W: Write>(opts: &SearchOpts, files: &[SessionFile], em: &mut Emitter<W>) -> Result<()> {
    anyhow::ensure!(!opts.queries.is_empty(), "search query cannot be empty");

    let start = std::time::Instant::now();
    let matcher = Matcher::new(&opts.queries, opts.is_regex, opts.and_mode)?;

    let filtered: Vec<&SessionFile> = files
        .iter()
        .filter(|f| {
            if let Some(proj) = &opts.project {
                if !f.project_name.to_lowercase().contains(&proj.to_lowercase()) {
                    return false;
                }
            }
            if let Some(exc) = &opts.exclude_session {
                if f.session_id.starts_with(exc.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();

    let hit_count = AtomicUsize::new(0);
    let max = opts.max_results;

    let results: Vec<Vec<SearchRecord>> = filtered
        .par_iter()
        .map(|file| {
            if max > 0 && hit_count.load(Ordering::Relaxed) >= max {
                return vec![];
            }
            search_file(file, &matcher, opts, &hit_count, max)
        })
        .collect();

    let mut count = 0usize;
    'outer: for hits in &results {
        for rec in hits {
            if !em.emit(rec)? {
                break 'outer;
            }
            count += 1;
        }
    }

    let summary = SearchSummary {
        record_type: "summary",
        query: opts.queries.join(", "),
        count,
        files_scanned: filtered.len(),
        elapsed_ms: start.elapsed().as_millis(),
    };
    em.emit(&summary)?;

    em.flush()?;
    Ok(())
}

// ── Per-file search ────────────────────────────────────────────────────────

fn search_file(
    file: &SessionFile,
    matcher: &Matcher,
    opts: &SearchOpts,
    hit_count: &AtomicUsize,
    max: usize,
) -> Vec<SearchRecord> {
    let mut hits = Vec::new();

    let Ok(f) = std::fs::File::open(&file.path) else { return hits };
    let reader = std::io::BufReader::with_capacity(256 * 1024, f);

    use std::io::BufRead;
    for (line_num, line) in reader.lines().enumerate() {
        if max > 0 && hit_count.load(Ordering::Relaxed) >= max {
            break;
        }

        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }

        let Ok(record) = serde_json::from_str::<Record>(&line) else { continue };
        let Some(msg) = record.as_message() else { continue };

        // -- filters --

        if let Some(role) = &opts.role {
            if record.role() != role.as_str() {
                continue;
            }
        }

        if let Some(tool_name) = &opts.tool {
            let tools = msg.tool_names();
            if !tools.iter().any(|t| t.to_lowercase().contains(&tool_name.to_lowercase())) {
                continue;
            }
        }

        if let Some(after) = &opts.after {
            if let Some(ts) = &msg.timestamp {
                if ts.as_str() < after.as_str() {
                    continue;
                }
            }
        }

        if let Some(before) = &opts.before {
            if let Some(ts) = &msg.timestamp {
                if ts.as_str() > before.as_str() {
                    continue;
                }
            }
        }

        if let Some(branch) = &opts.branch {
            match &msg.git_branch {
                Some(gb) if gb.to_lowercase().contains(&branch.to_lowercase()) => {}
                _ => continue,
            }
        }

        if let Some(file_path) = &opts.file {
            if !msg.touches_file(file_path) {
                continue;
            }
        }

        // -- select search text --

        let text = if opts.thinking_only {
            msg.thinking_content()
        } else if opts.no_thinking {
            msg.text_no_thinking()
        } else if opts.tool_input {
            msg.tool_input_content()
        } else {
            msg.full_content()
        };

        if text.is_empty() {
            continue;
        }

        if !opts.include_smc && text.contains(SMC_TAG) {
            continue;
        }

        // -- match --

        if let Some(matched) = matcher.first_match(&text) {
            hit_count.fetch_add(1, Ordering::Relaxed);

            let preview: String = text.chars().take(500).collect();

            hits.push(SearchRecord {
                record_type: "match",
                project: file.project_name.clone(),
                session_id: file.session_id.clone(),
                line: line_num + 1,
                role: record.role().to_string(),
                timestamp: msg.timestamp.clone(),
                matched_query: matched,
                text: preview,
                tool_names: msg.tool_names().into_iter().map(String::from).collect(),
                git_branch: msg.git_branch.clone(),
            });
        }
    }

    hits
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_plain_or() {
        let m = Matcher::new(&["foo".into(), "bar".into()], false, false).unwrap();
        assert!(m.first_match("hello foo world").is_some());
        assert!(m.first_match("hello bar world").is_some());
        assert!(m.first_match("hello baz world").is_none());
    }

    #[test]
    fn matcher_plain_and() {
        let m = Matcher::new(&["foo".into(), "bar".into()], false, true).unwrap();
        assert!(m.first_match("foo and bar").is_some());
        assert!(m.first_match("foo only").is_none());
    }

    #[test]
    fn matcher_regex() {
        let m = Matcher::new(&["fn\\s+\\w+".into()], true, false).unwrap();
        assert!(m.first_match("pub fn main()").is_some());
        assert!(m.first_match("no function here").is_none());
    }
}
