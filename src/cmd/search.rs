/// smc search — parallel full-text search across Claude Code conversation logs.
use std::collections::{HashMap, HashSet};
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
    /// BM25 relevance (highest first). Implies scoring.
    Relevance,
}

/// Collapse matches into groups instead of listing every hit.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[clap(rename_all = "kebab-case")]
pub enum GroupMode {
    /// One group per session.
    Session,
    /// One group per conversation thread (root of the parentUuid chain).
    Thread,
}

#[derive(Clone)]
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
    pub max_results: usize,
    pub include_smc: bool,
    pub exclude_session: Option<String>,
    /// Max characters per match snippet (centered on the match).
    pub snippet_len: usize,
    /// Result ordering.
    pub sort: SortMode,
    /// Collapse matches into groups (by session or thread) instead of listing each.
    pub group_by: Option<GroupMode>,
    /// Sample matches to include per group.
    pub group_samples: usize,
    /// Compute and emit a BM25 relevance score per match.
    pub score: bool,
    /// Inline N surrounding messages per match (0 = none). Context messages
    /// ignore role/tool/date filters — they are the conversation, not the
    /// filtered view.
    pub context: usize,
    /// Join all query words into ONE exact phrase (substring match).
    pub phrase: bool,
    /// Collapse matches whose snippets are identical (repeated CLAUDE.md
    /// echoes, boilerplate) — keeps the first per sort order.
    pub dedupe: bool,
    /// Skip session files modified within the last N seconds — i.e. the
    /// live conversation that is invoking smc, whose own commands would
    /// otherwise self-match. None = off. (Transcript writes are debounced,
    /// so the window needs slack; the CLI default is 120s.)
    pub exclude_live: Option<u64>,
}

/// Character cap for each inline context message preview.
const CTX_TEXT_CHARS: usize = 200;

// ── Records ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
struct SearchRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    project: String,
    session_id: String,
    line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    uuid: Option<String>,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    matched_query: String,
    /// BM25 relevance score (present when scoring is enabled).
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
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
    /// Surrounding messages (populated by --context N).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context_before: Vec<CtxMsg>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    context_after: Vec<CtxMsg>,
    /// Thread root (root of the parentUuid chain) — used for `--group-by thread`,
    /// not serialized on individual match records.
    #[serde(skip)]
    thread_root: Option<String>,
    /// Document length in words and per-term frequencies — BM25 inputs, computed
    /// during the scan and consumed when scoring; never serialized.
    #[serde(skip)]
    doc_len: usize,
    #[serde(skip)]
    tfs: Vec<usize>,
}

/// One surrounding message in a match's inline context.
#[derive(Serialize, Debug, Clone)]
struct CtxMsg {
    line: usize,
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    text: String,
}

#[derive(Serialize, Debug)]
struct GroupRecord {
    #[serde(rename = "type")]
    record_type: &'static str,
    group_by: &'static str,
    key: String,
    project: String,
    session_id: String,
    hits: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_ts: Option<String>,
    samples: Vec<GroupSample>,
}

#[derive(Serialize, Debug)]
struct GroupSample {
    line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    text: String,
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
    /// Matches collapsed by --dedupe (present only when deduping).
    #[serde(skip_serializing_if = "Option::is_none")]
    deduped: Option<usize>,
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

    /// One-pass match + per-term frequencies. In plain mode the text is
    /// lowercased exactly once and shared between matching and TF counting
    /// (the old code lowercased twice per line when scoring). `tfs` is empty
    /// when `scoring` is false.
    fn scan(&self, text: &str, scoring: bool) -> (Option<MatchInfo>, Vec<usize>) {
        if self.regexes.is_empty() {
            let lower = text.to_lowercase();
            let info = self.plain_match(&lower);
            let tfs = if scoring {
                self.plains.iter().map(|q| lower.matches(q.as_str()).count()).collect()
            } else {
                Vec::new()
            };
            (info, tfs)
        } else {
            let info = self.regex_match(text);
            let tfs = if scoring {
                self.regexes.iter().map(|re| re.find_iter(text).count()).collect()
            } else {
                Vec::new()
            };
            (info, tfs)
        }
    }

    /// Number of query terms (plain terms or regexes).
    fn term_count(&self) -> usize {
        if self.regexes.is_empty() { self.plains.len() } else { self.regexes.len() }
    }

    /// Plain-term matching over already-lowercased text.
    fn plain_match(&self, lower: &str) -> Option<MatchInfo> {
        if self.and_mode {
            let mut min_pos = usize::MAX;
            for q in &self.plains {
                {
                    let b = lower.find(q.as_str())?;
                    min_pos = min_pos.min(lower[..b].chars().count())
                }
            }
            Some(MatchInfo {
                matched: self.plains.join(" + "),
                char_pos: if min_pos == usize::MAX { 0 } else { min_pos },
            })
        } else {
            let mut best: Option<MatchInfo> = None;
            for q in &self.plains {
                if let Some(b) = lower.find(q.as_str()) {
                    let pos = lower[..b].chars().count();
                    if best.as_ref().is_none_or(|x| pos < x.char_pos) {
                        best = Some(MatchInfo { matched: q.clone(), char_pos: pos });
                    }
                }
            }
            best
        }
    }

    fn regex_match(&self, text: &str) -> Option<MatchInfo> {
        if self.and_mode {
            let mut hits = Vec::new();
            let mut min_pos = usize::MAX;
            for re in &self.regexes {
                {
                    let m = re.find(text)?;
                    hits.push(m.as_str().to_string());
                    min_pos = min_pos.min(text[..m.start()].chars().count());
                }
            }
            Some(MatchInfo {
                matched: hits.join(" + "),
                char_pos: if min_pos == usize::MAX { 0 } else { min_pos },
            })
        } else {
            let mut best: Option<MatchInfo> = None;
            for re in &self.regexes {
                if let Some(m) = re.find(text) {
                    let pos = text[..m.start()].chars().count();
                    if best.as_ref().is_none_or(|b| pos < b.char_pos) {
                        best = Some(MatchInfo { matched: m.as_str().to_string(), char_pos: pos });
                    }
                }
            }
            best
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

// ── BM25 scoring ─────────────────────────────────────────────────────────────

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// Corpus statistics gathered over the searched documents (per-term document
/// frequency, document count, and total length), reduced across files.
#[derive(Default)]
struct CorpusStats {
    docs: usize,
    total_len: usize,
    /// Document frequency per query term (how many docs contain it).
    df: Vec<usize>,
}

impl CorpusStats {
    fn new(terms: usize) -> Self {
        Self { docs: 0, total_len: 0, df: vec![0; terms] }
    }

    fn merge(&mut self, other: &CorpusStats) {
        self.docs += other.docs;
        self.total_len += other.total_len;
        if self.df.len() < other.df.len() {
            self.df.resize(other.df.len(), 0);
        }
        for (i, c) in other.df.iter().enumerate() {
            self.df[i] += c;
        }
    }

    /// BM25 score for one document given its per-term frequencies and length.
    fn bm25(&self, tfs: &[usize], doc_len: usize) -> f64 {
        if self.docs == 0 {
            return 0.0;
        }
        let n = self.docs as f64;
        let avgdl = (self.total_len as f64 / n).max(1.0);
        let mut score = 0.0;
        for (i, &tf) in tfs.iter().enumerate() {
            if tf == 0 {
                continue;
            }
            let df = *self.df.get(i).unwrap_or(&0) as f64;
            // Robertson/Sparck-Jones IDF with the +1 guard (always ≥ 0).
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            let f = tf as f64;
            let denom = f + BM25_K1 * (1.0 - BM25_B + BM25_B * doc_len as f64 / avgdl);
            score += idf * (f * (BM25_K1 + 1.0)) / denom;
        }
        score
    }
}

// ── run ────────────────────────────────────────────────────────────────────

/// Returns whether anything matched (drives the process exit code).
pub fn run<W: Write>(opts: &SearchOpts, files: &[SessionFile], em: &mut Emitter<W>) -> Result<bool> {
    anyhow::ensure!(!opts.queries.is_empty(), "search query cannot be empty");

    let start = std::time::Instant::now();

    // --phrase joins every query word into one exact substring.
    let queries: Vec<String> = if opts.phrase {
        vec![opts.queries.join(" ")]
    } else {
        opts.queries.clone()
    };
    let matcher = Matcher::new(&queries, opts.is_regex, opts.and_mode)?;

    // Normalize a date-only --before so it includes the whole named day
    // (lexically, "2026-07-01T10:00" > "2026-07-01" would otherwise exclude it).
    let normalized;
    let opts = if let Some(b) = &opts.before {
        normalized = SearchOpts {
            before: Some(crate::util::dates::normalize_before(b.clone())),
            ..opts.clone()
        };
        &normalized
    } else {
        opts
    };

    let now = std::time::SystemTime::now();
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
            if let Some(window) = opts.exclude_live {
                // Unknown or future mtimes count as live (conservative).
                let live = match f.modified {
                    Some(m) => now
                        .duration_since(m)
                        .map(|d| d.as_secs() < window)
                        .unwrap_or(true),
                    None => true,
                };
                if live {
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
    let scoring = opts.score || opts.sort == SortMode::Relevance;

    let results: Vec<(Vec<SearchRecord>, CorpusStats)> = filtered
        .par_iter()
        .map(|file| search_file(file, &matcher, opts, scoring))
        .collect();

    // Reduce corpus stats across files (only meaningful when scoring).
    let mut corpus = CorpusStats::new(matcher.term_count());
    let mut all: Vec<SearchRecord> = Vec::new();
    for (hits, stats) in results {
        corpus.merge(&stats);
        all.extend(hits);
    }
    let total_matched = all.len();

    // Score (BM25) using the now-complete corpus stats.
    if scoring {
        for rec in &mut all {
            rec.score = Some((corpus.bm25(&rec.tfs, rec.doc_len) * 10000.0).round() / 10000.0);
        }
    }

    // Sort per mode (timestamps are ISO-8601 → lexically ordered; missing
    // timestamps sort last under recency). Document order is left as found.
    match opts.sort {
        SortMode::Recency => all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp)),
        SortMode::Oldest => all.sort_by(|a, b| a.timestamp.cmp(&b.timestamp)),
        SortMode::Relevance => all.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
        }),
        SortMode::Document => {}
    }

    // Dedupe AFTER sorting (keeps the best/first per sort order) and BEFORE
    // capping, so duplicates don't eat --max slots.
    let mut deduped = 0usize;
    if opts.dedupe {
        let mut seen: HashSet<String> = HashSet::new();
        let pre = all.len();
        all.retain(|r| seen.insert(r.text.clone()));
        deduped = pre - all.len();
    }

    let (count, intended, capped) = if let Some(mode) = opts.group_by {
        emit_groups(&all, mode, max, opts.group_samples, em)?
    } else {
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
        (count, intended, capped)
    };

    // A single multi-word query is an exact-substring match — a common trap
    // when the caller expected web-search semantics. Say so on zero hits.
    if total_matched == 0 && !opts.is_regex && !opts.phrase && queries.len() == 1
        && queries[0].contains(char::is_whitespace)
    {
        em.warn(
            None,
            "0 matches: a multi-word query matches as ONE exact substring. \
             Pass words as separate arguments for OR (or ranked --sort relevance) \
             search, or use --phrase/-F to make exact-phrase intent explicit.",
        );
    }

    let summary = SearchSummary {
        record_type: "summary",
        query: queries.join(", "),
        // In group mode `count` is groups emitted; total_matched is always matches.
        count,
        total_matched,
        files_scanned: filtered.len(),
        // Budget cut emission short if we didn't write everything we meant to.
        truncated: count < intended,
        // max-results hid additional matches/groups.
        capped,
        deduped: opts.dedupe.then_some(deduped),
        elapsed_ms: start.elapsed().as_millis(),
    };
    // Always emit the summary, even when the budget is exhausted — it's the
    // record that tells the consumer the output was incomplete.
    em.emit_always(&summary)?;

    em.flush()?;
    Ok(total_matched > 0)
}

// ── Grouping ─────────────────────────────────────────────────────────────────

struct GroupAccum {
    project: String,
    session_id: String,
    hits: usize,
    first_ts: Option<String>,
    last_ts: Option<String>,
    samples: Vec<GroupSample>,
}

/// Collapse already-sorted matches into groups (by session or thread) and emit a
/// `group` record per group. Returns (emitted, intended, capped). Group order
/// follows first appearance in the sorted match list (so it inherits --sort).
fn emit_groups<W: Write>(
    all: &[SearchRecord],
    mode: GroupMode,
    max: usize,
    samples_per_group: usize,
    em: &mut Emitter<W>,
) -> Result<(usize, usize, bool)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, GroupAccum> = HashMap::new();

    for rec in all {
        let key = match mode {
            GroupMode::Session => rec.session_id.clone(),
            GroupMode::Thread => rec.thread_root.clone().unwrap_or_else(|| rec.session_id.clone()),
        };
        let g = groups.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            GroupAccum {
                project: rec.project.clone(),
                session_id: rec.session_id.clone(),
                hits: 0,
                first_ts: None,
                last_ts: None,
                samples: Vec::new(),
            }
        });
        g.hits += 1;
        if let Some(ts) = &rec.timestamp {
            if g.first_ts.as_deref().is_none_or(|x| ts.as_str() < x) {
                g.first_ts = Some(ts.clone());
            }
            if g.last_ts.as_deref().is_none_or(|x| ts.as_str() > x) {
                g.last_ts = Some(ts.clone());
            }
        }
        if g.samples.len() < samples_per_group {
            g.samples.push(GroupSample {
                line: rec.line,
                timestamp: rec.timestamp.clone(),
                text: rec.text.clone(),
            });
        }
    }

    let total_groups = order.len();
    let capped = max > 0 && total_groups > max;
    let intended = if max > 0 { total_groups.min(max) } else { total_groups };

    let group_by = match mode {
        GroupMode::Session => "session",
        GroupMode::Thread => "thread",
    };

    let mut count = 0usize;
    for key in order.into_iter().take(intended) {
        let g = groups.remove(&key).expect("group key present");
        let rec = GroupRecord {
            record_type: "group",
            group_by,
            key,
            project: g.project,
            session_id: g.session_id,
            hits: g.hits,
            first_ts: g.first_ts,
            last_ts: g.last_ts,
            samples: g.samples,
        };
        if !em.emit(&rec)? {
            break;
        }
        count += 1;
    }

    Ok((count, intended, capped))
}

// ── Per-file search ────────────────────────────────────────────────────────

fn search_file(
    file: &SessionFile,
    matcher: &Matcher,
    opts: &SearchOpts,
    scoring: bool,
) -> (Vec<SearchRecord>, CorpusStats) {
    let mut hits: Vec<SearchRecord> = Vec::new();
    let mut corpus = CorpusStats::new(matcher.term_count());
    // For `--group-by thread` we need the full uuid→parent map of the session to
    // resolve each match's thread root, so build it for every message (pre-filter).
    let want_thread = opts.group_by == Some(GroupMode::Thread);
    let mut uuid2parent: HashMap<String, Option<String>> = HashMap::new();

    // --context N: rolling window of preceding messages + matches still
    // waiting for their trailing context.
    let want_ctx = opts.context > 0;
    let mut ctx_before: std::collections::VecDeque<CtxMsg> = std::collections::VecDeque::new();
    // (index into `hits`, messages still owed)
    let mut ctx_pending: Vec<(usize, usize)> = Vec::new();

    let Ok(f) = std::fs::File::open(&file.path) else { return (hits, corpus) };
    let reader = std::io::BufReader::with_capacity(256 * 1024, f);

    use std::io::BufRead;
    for (line_num, line) in reader.lines().enumerate() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }

        let Ok(record) = serde_json::from_str::<Record>(&line) else { continue };
        let Some(msg) = record.as_message() else { continue };
        let line_no = line_num + 1;

        if want_thread {
            if let Some(u) = &msg.uuid {
                uuid2parent.insert(u.clone(), msg.parent_uuid_str());
            }
        }

        // Context tracks EVERY message, before filters: it is the surrounding
        // conversation, not the filtered view.
        let ctx = want_ctx.then(|| CtxMsg {
            line: line_no,
            role: record.role().to_string(),
            timestamp: msg.timestamp.clone(),
            text: msg.full_content().chars().take(CTX_TEXT_CHARS).collect(),
        });

        // Deliver trailing context owed to earlier matches (before this
        // message can itself match, so a match never appears in its own after).
        if let Some(ctx) = &ctx {
            ctx_pending.retain_mut(|(idx, remaining)| {
                hits[*idx].context_after.push(ctx.clone());
                *remaining -= 1;
                *remaining > 0
            });
        }

        // Filters `break` out of this block; the tail of the loop (context
        // window upkeep) still runs for filtered-out messages.
        'this_msg: {
            let mut tools_cache: Option<Vec<String>> = None;

            if let Some(role) = &opts.role {
                if record.role() != role.as_str() {
                    break 'this_msg;
                }
            }

            if let Some(tool_name) = &opts.tool {
                let tools: Vec<String> =
                    msg.tool_names().into_iter().map(String::from).collect();
                let want = tool_name.to_lowercase();
                if !tools.iter().any(|t| t.to_lowercase().contains(&want)) {
                    break 'this_msg;
                }
                tools_cache = Some(tools);
            }

            // Date filters: messages without a timestamp can't be placed in
            // the window, so they are excluded when a date filter is active.
            if opts.after.is_some() || opts.before.is_some() {
                let Some(ts) = &msg.timestamp else { break 'this_msg };
                if let Some(after) = &opts.after {
                    if ts.as_str() < after.as_str() {
                        break 'this_msg;
                    }
                }
                if let Some(before) = &opts.before {
                    if ts.as_str() > before.as_str() {
                        break 'this_msg;
                    }
                }
            }

            if let Some(branch) = &opts.branch {
                match &msg.git_branch {
                    Some(gb) if gb.to_lowercase().contains(&branch.to_lowercase()) => {}
                    _ => break 'this_msg,
                }
            }

            if let Some(file_path) = &opts.file {
                if !msg.touches_file(file_path) {
                    break 'this_msg;
                }
            }

            // -- select search text --

            let text = if opts.tool_input {
                msg.tool_input_content()
            } else {
                msg.full_content()
            };

            if text.is_empty() {
                break 'this_msg;
            }

            if !opts.include_smc && text.contains(SMC_TAG) {
                break 'this_msg;
            }

            // -- match + corpus stats in one pass (BM25 counts every searched
            //    doc, not just matches) --

            let (info, tfs) = matcher.scan(&text, scoring);
            let doc_len = if scoring { text.split_whitespace().count() } else { 0 };
            if scoring {
                corpus.docs += 1;
                corpus.total_len += doc_len;
                for (i, &tf) in tfs.iter().enumerate() {
                    if tf > 0 {
                        corpus.df[i] += 1;
                    }
                }
            }

            if let Some(info) = info {
                let chars: Vec<char> = text.chars().collect();
                let snippet = make_snippet(&chars, info.char_pos, opts.snippet_len);
                let tool_names = tools_cache.unwrap_or_else(|| {
                    msg.tool_names().into_iter().map(String::from).collect()
                });

                hits.push(SearchRecord {
                    record_type: "match",
                    project: file.project_name.clone(),
                    session_id: file.session_id.clone(),
                    line: line_no,
                    uuid: msg.uuid.clone(),
                    role: record.role().to_string(),
                    timestamp: msg.timestamp.clone(),
                    matched_query: info.matched,
                    score: None,
                    text: snippet,
                    match_offset: info.char_pos,
                    msg_chars: chars.len(),
                    tool_names,
                    git_branch: msg.git_branch.clone(),
                    context_before: ctx_before.iter().cloned().collect(),
                    context_after: Vec::new(),
                    thread_root: None,
                    doc_len,
                    tfs,
                });
                if want_ctx {
                    ctx_pending.push((hits.len() - 1, opts.context));
                }
            }
        }

        // Slide the context window (runs for every message, filtered or not).
        if let Some(ctx) = ctx {
            ctx_before.push_back(ctx);
            if ctx_before.len() > opts.context {
                ctx_before.pop_front();
            }
        }
    }

    // Resolve each match's thread root by walking the parentUuid chain to the top.
    if want_thread {
        for h in &mut hits {
            if let Some(u) = &h.uuid {
                h.thread_root = Some(resolve_thread_root(u, &uuid2parent));
            }
        }
    }

    (hits, corpus)
}

/// Walk the parentUuid chain from `start` up to the topmost known message.
fn resolve_thread_root(start: &str, map: &HashMap<String, Option<String>>) -> String {
    let mut cur = start.to_string();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(cur.clone()) {
            break; // cycle guard
        }
        match map.get(&cur) {
            Some(Some(parent)) if map.contains_key(parent) => cur = parent.clone(),
            _ => break,
        }
    }
    cur
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn first_match(m: &Matcher, text: &str) -> Option<MatchInfo> {
        m.scan(text, false).0
    }

    #[test]
    fn matcher_plain_or() {
        let m = Matcher::new(&["foo".into(), "bar".into()], false, false).unwrap();
        assert!(first_match(&m, "hello foo world").is_some());
        assert!(first_match(&m, "hello bar world").is_some());
        assert!(first_match(&m, "hello baz world").is_none());
    }

    #[test]
    fn matcher_plain_and() {
        let m = Matcher::new(&["foo".into(), "bar".into()], false, true).unwrap();
        assert!(first_match(&m, "foo and bar").is_some());
        assert!(first_match(&m, "foo only").is_none());
    }

    #[test]
    fn matcher_regex() {
        let m = Matcher::new(&["fn\\s+\\w+".into()], true, false).unwrap();
        assert!(first_match(&m, "pub fn main()").is_some());
        assert!(first_match(&m, "no function here").is_none());
    }

    #[test]
    fn matcher_regex_and_mode() {
        let m = Matcher::new(&["fn\\s+\\w+".into(), "pub".into()], true, true).unwrap();
        assert!(first_match(&m, "pub fn main()").is_some());
        assert!(first_match(&m, "fn main()").is_none(), "AND requires every regex");
    }

    #[test]
    fn scan_counts_occurrences() {
        let m = Matcher::new(&["x".into(), "y".into()], false, false).unwrap();
        assert_eq!(m.scan("x x x y", true).1, vec![3, 1]);
        assert_eq!(m.scan("nothing here", true).1, vec![0, 0]);
        assert!(m.scan("x only", false).1.is_empty(), "no TF work when not scoring");
    }

    #[test]
    fn match_is_case_insensitive_in_plain_mode() {
        let m = Matcher::new(&["FooBar".into()], false, false).unwrap();
        let info = first_match(&m, "prefix FOOBAR suffix").unwrap();
        assert_eq!(info.char_pos, 7);
    }

    // ── make_snippet: boundary + multibyte adversarial cases ──────────────

    #[test]
    fn snippet_shorter_than_window_is_untouched() {
        let chars: Vec<char> = "short text".chars().collect();
        assert_eq!(make_snippet(&chars, 0, 100), "short text");
    }

    #[test]
    fn snippet_zero_max_means_unlimited() {
        let chars: Vec<char> = "abc".repeat(100).chars().collect();
        assert_eq!(make_snippet(&chars, 5, 0).chars().count(), 300);
    }

    #[test]
    fn snippet_centered_with_markers_both_sides() {
        let chars: Vec<char> = ('a'..='z').cycle().take(100).collect();
        let s = make_snippet(&chars, 50, 10);
        assert!(s.starts_with('…') && s.ends_with('…'));
        assert_eq!(s.chars().count(), 12); // 10 + two markers
    }

    #[test]
    fn snippet_match_at_start_and_end() {
        let chars: Vec<char> = ('a'..='z').cycle().take(100).collect();
        let at_start = make_snippet(&chars, 0, 10);
        assert!(!at_start.starts_with('…') && at_start.ends_with('…'));
        let at_end = make_snippet(&chars, 99, 10);
        assert!(at_end.starts_with('…') && !at_end.ends_with('…'));
    }

    #[test]
    fn snippet_multibyte_never_splits_or_panics() {
        // Emoji + CJK + combining marks — every boundary is a char boundary.
        let text = "🎉🚀日本語テキストe\u{301}と絵文字🧪の混在テスト".repeat(20);
        let chars: Vec<char> = text.chars().collect();
        for pos in [0, 1, chars.len() / 2, chars.len() - 1] {
            for max in [1, 2, 3, 10, chars.len(), chars.len() + 5] {
                let s = make_snippet(&chars, pos, max);
                assert!(!s.is_empty());
            }
        }
    }

    #[test]
    fn thread_root_cycle_does_not_hang() {
        // Adversarial: corrupted parent chain A→B→A must terminate.
        let mut map = HashMap::new();
        map.insert("A".to_string(), Some("B".to_string()));
        map.insert("B".to_string(), Some("A".to_string()));
        let root = resolve_thread_root("A", &map);
        assert!(root == "A" || root == "B");
    }

    #[test]
    fn thread_root_walks_to_top() {
        let mut map = HashMap::new();
        map.insert("C".to_string(), Some("B".to_string()));
        map.insert("B".to_string(), Some("A".to_string()));
        map.insert("A".to_string(), None);
        assert_eq!(resolve_thread_root("C", &map), "A");
    }

    #[test]
    fn bm25_hand_computed() {
        // N=3 docs, term df=2, total_len=9 → avgdl=3, IDF=ln(1.6)=0.4700.
        let mut c = CorpusStats::new(1);
        c.docs = 3;
        c.total_len = 9;
        c.df = vec![2];
        let hi = c.bm25(&[3], 3); // tf=3 → 0.4700 * (3*2.2)/(3+1.2) = 0.73858
        let lo = c.bm25(&[1], 3); // tf=1 → 0.4700 * (1*2.2)/(1+1.2) = 0.47000
        assert!(hi > lo, "more occurrences must score higher");
        assert!((hi - 0.73858).abs() < 0.001, "hi was {hi}");
        assert!((lo - 0.47000).abs() < 0.001, "lo was {lo}");
    }

    #[test]
    fn bm25_zero_when_empty_corpus() {
        let c = CorpusStats::new(1);
        assert_eq!(c.bm25(&[5], 10), 0.0);
    }
}
