/// smc search — parallel full-text search across Claude Code conversation logs.
use std::io::Write;

use anyhow::Result;
use rayon::prelude::*;
use regex::Regex;
use serde::Serialize;

use crate::models::Record;
use crate::output::{Emitter, SMC_TAG};
use crate::util::discover::SessionFile;

// ── Opts ───────────────────────────────────────────────────────────────────

/// Result ordering. `Document` keeps file/line order (deterministic); `Recency`
/// and `Oldest` sort by message timestamp.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[clap(rename_all = "kebab-case")]
pub enum SortMode {
    #[default]
    Document,
    Recency,
    Oldest,
}

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
    /// Max characters per match snippet (centered on the match).
    pub snippet_len: usize,
    /// Result ordering.
    pub sort: SortMode,
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
    /// Snippet centered on the match (with `…` markers when the message is longer).
    text: String,
    /// Character offset of the match within the full message.
    match_offset: usize,
    /// Full message length in characters (so the consumer knows how much `text` omits).
    msg_chars: usize,
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
    /// Matches actually emitted.
    count: usize,
    /// Matches found (≥ `count` when token-budget truncation cut emission short).
    total_matched: usize,
    files_scanned: usize,
    /// True when the token budget stopped emission before all matches were written.
    truncated: bool,
    /// True when `--max-results` may have hidden additional matches.
    capped: bool,
    elapsed_ms: u128,
}

// ── Matcher ────────────────────────────────────────────────────────────────

/// A match: which query hit, and the character offset of the earliest hit
/// (used to center the snippet).
struct MatchInfo {
    matched: String,
    char_pos: usize,
}

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

    /// In OR mode: the matching query whose hit appears earliest in `text`.
    /// In AND mode: all queries must hit; offset is the earliest among them.
    fn first_match(&self, text: &str) -> Option<MatchInfo> {
        if self.and_mode {
            return self.all_match(text);
        }
        let mut best: Option<MatchInfo> = None;
        if !self.regexes.is_empty() {
            for re in &self.regexes {
                if let Some(m) = re.find(text) {
                    let pos = text[..m.start()].chars().count();
                    if best.as_ref().is_none_or(|b| pos < b.char_pos) {
                        best = Some(MatchInfo { matched: m.as_str().to_string(), char_pos: pos });
                    }
                }
            }
        } else {
            let lower = text.to_lowercase();
            for q in &self.plains {
                if let Some(b) = lower.find(q.as_str()) {
                    let pos = lower[..b].chars().count();
                    if best.as_ref().is_none_or(|x| pos < x.char_pos) {
                        best = Some(MatchInfo { matched: q.clone(), char_pos: pos });
                    }
                }
            }
        }
        best
    }

    fn all_match(&self, text: &str) -> Option<MatchInfo> {
        let mut min_pos = usize::MAX;
        if !self.regexes.is_empty() {
            let mut hits = Vec::new();
            for re in &self.regexes {
                match re.find(text) {
                    Some(m) => {
                        hits.push(m.as_str().to_string());
                        min_pos = min_pos.min(text[..m.start()].chars().count());
                    }
                    None => return None,
                }
            }
            Some(MatchInfo { matched: hits.join(" + "), char_pos: if min_pos == usize::MAX { 0 } else { min_pos } })
        } else {
            let lower = text.to_lowercase();
            for q in &self.plains {
                match lower.find(q.as_str()) {
                    Some(b) => min_pos = min_pos.min(lower[..b].chars().count()),
                    None => return None,
                }
            }
            Some(MatchInfo { matched: self.plains.join(" + "), char_pos: if min_pos == usize::MAX { 0 } else { min_pos } })
        }
    }
}

/// Build a snippet of at most `max_chars` characters centered on `match_pos`,
/// adding `…` markers when the message extends past the window. Operates on
/// chars (never bytes) so it can't split a multi-byte boundary.
fn make_snippet(chars: &[char], match_pos: usize, max_chars: usize) -> String {
    if max_chars == 0 || chars.len() <= max_chars {
        return chars.iter().collect();
    }
    let half = max_chars / 2;
    let mut start = match_pos.saturating_sub(half);
    if start + max_chars > chars.len() {
        start = chars.len() - max_chars;
    }
    let end = start + max_chars;
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.extend(chars[start..end].iter());
    if end < chars.len() {
        s.push('…');
    }
    s
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

    let max = opts.max_results;

    // Search exhaustively (parallel collect preserves file order → deterministic
    // "document" order). We no longer early-exit at max_results: to honor a sort
    // we need every match before truncating, and this makes total_matched exact
    // and the result set reproducible (the old atomic cap was racy).
    let results: Vec<Vec<SearchRecord>> = filtered
        .par_iter()
        .map(|file| search_file(file, &matcher, opts))
        .collect();

    let mut all: Vec<SearchRecord> = results.into_iter().flatten().collect();
    let total_matched = all.len();

    // Sort per mode (timestamps are ISO-8601 → lexically ordered; missing
    // timestamps sort last under recency). Document order is left as found.
    match opts.sort {
        SortMode::Recency => all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp)),
        SortMode::Oldest => all.sort_by(|a, b| a.timestamp.cmp(&b.timestamp)),
        SortMode::Document => {}
    }

    // Cap AFTER sorting so "the N most recent" is honored, not "N arbitrary".
    let capped = max > 0 && all.len() > max;
    if max > 0 {
        all.truncate(max);
    }
    let intended = all.len();

    let mut count = 0usize;
    for rec in &all {
        if !em.emit(rec)? {
            break;
        }
        count += 1;
    }

    let summary = SearchSummary {
        record_type: "summary",
        query: opts.queries.join(", "),
        count,
        total_matched,
        files_scanned: filtered.len(),
        // Budget cut emission short if we didn't write every match we meant to.
        truncated: count < intended,
        // max-results hid additional matches.
        capped,
        elapsed_ms: start.elapsed().as_millis(),
    };
    // Always emit the summary, even when the budget is exhausted — it's the
    // record that tells the consumer the output was incomplete.
    em.emit_always(&summary)?;

    em.flush()?;
    Ok(())
}

// ── Per-file search ────────────────────────────────────────────────────────

fn search_file(file: &SessionFile, matcher: &Matcher, opts: &SearchOpts) -> Vec<SearchRecord> {
    let mut hits = Vec::new();

    let Ok(f) = std::fs::File::open(&file.path) else { return hits };
    let reader = std::io::BufReader::with_capacity(256 * 1024, f);

    use std::io::BufRead;
    for (line_num, line) in reader.lines().enumerate() {
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

        if let Some(info) = matcher.first_match(&text) {
            let chars: Vec<char> = text.chars().collect();
            let snippet = make_snippet(&chars, info.char_pos, opts.snippet_len);

            hits.push(SearchRecord {
                record_type: "match",
                project: file.project_name.clone(),
                session_id: file.session_id.clone(),
                line: line_num + 1,
                role: record.role().to_string(),
                timestamp: msg.timestamp.clone(),
                matched_query: info.matched,
                text: snippet,
                match_offset: info.char_pos,
                msg_chars: chars.len(),
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
