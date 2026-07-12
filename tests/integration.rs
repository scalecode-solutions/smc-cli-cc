//! End-to-end tests: synthetic JSONL corpora → real command pipeline →
//! captured JSONL output. Includes adversarial corpora (garbage lines, CRLF,
//! huge messages, self-referential smc output).
use std::path::PathBuf;

use serde_json::{json, Value};
use smc::cmd;
use smc::cmd::search::{GroupMode, SearchOpts, SortMode};
use smc::output::Emitter;
use smc::util::discover::SessionFile;

// ── Harness ────────────────────────────────────────────────────────────────

struct TempCorpus {
    dir: PathBuf,
    files: Vec<SessionFile>,
}

impl TempCorpus {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("smc-it-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir, files: Vec::new() }
    }

    fn add_session(&mut self, session_id: &str, project: &str, lines: &[String]) {
        let path = self.dir.join(format!("{session_id}.jsonl"));
        let content = lines.join("\n");
        std::fs::write(&path, &content).unwrap();
        self.files.push(SessionFile {
            path,
            session_id: session_id.to_string(),
            project_name: project.to_string(),
            size_bytes: content.len() as u64,
            modified: None,
        });
    }
}

impl Drop for TempCorpus {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn user(uuid: &str, parent: Option<&str>, ts: &str, text: &str) -> String {
    json!({
        "type": "user", "uuid": uuid, "parentUuid": parent, "timestamp": ts,
        "message": {"role": "user", "content": text}
    })
    .to_string()
}

fn assistant(uuid: &str, parent: Option<&str>, ts: &str, text: &str) -> String {
    json!({
        "type": "assistant", "uuid": uuid, "parentUuid": parent, "timestamp": ts,
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}
    })
    .to_string()
}

fn tool_result(uuid: &str, ts: &str, text: &str) -> String {
    json!({
        "type": "user", "uuid": uuid, "timestamp": ts,
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1",
             "content": [{"type": "text", "text": text}]}
        ]}
    })
    .to_string()
}

fn system(uuid: &str, ts: &str, content: &str) -> String {
    json!({"type": "system", "uuid": uuid, "timestamp": ts, "content": content}).to_string()
}

fn opts(queries: &[&str]) -> SearchOpts {
    SearchOpts {
        queries: queries.iter().map(|s| s.to_string()).collect(),
        is_regex: false,
        and_mode: false,
        role: None,
        tool: None,
        project: None,
        after: None,
        before: None,
        branch: None,
        file: None,
        tool_input: false,
        max_results: 50,
        include_smc: false,
        exclude_session: None,
        snippet_len: 500,
        sort: SortMode::Document,
        group_by: None,
        group_samples: 3,
        score: false,
        context: 0,
        phrase: false,
        dedupe: false,
        exclude_live: None,
    }
}

/// Run a search, return (found, records-without-meta-header, summary).
fn search(o: &SearchOpts, files: &[SessionFile], budget: usize) -> (bool, Vec<Value>, Value) {
    let mut em = Emitter::capturing(budget);
    let found = cmd::search::run(o, files, &mut em).unwrap();
    let mut records = em.into_records();
    assert_eq!(records[0]["type"], "meta", "stream must start with the meta header");
    records.remove(0);
    let summary = records.pop().expect("summary record");
    assert_eq!(summary["type"], "summary", "stream must end with a summary");
    (found, records, summary)
}

// ── Search basics ──────────────────────────────────────────────────────────

#[test]
fn search_finds_matches_and_reports_honestly() {
    let mut c = TempCorpus::new("basic");
    c.add_session(
        "aaaa1111",
        "projA",
        &[
            user("u1", None, "2026-01-01T10:00:00Z", "let's fix the flux capacitor"),
            assistant("a1", Some("u1"), "2026-01-01T10:00:05Z", "the flux capacitor is fixed"),
        ],
    );
    c.add_session(
        "bbbb2222",
        "projB",
        &[user("u2", None, "2026-01-02T10:00:00Z", "unrelated message")],
    );

    let (found, records, summary) = search(&opts(&["flux capacitor"]), &c.files, 0);
    assert!(found);
    assert_eq!(records.len(), 2);
    assert_eq!(summary["total_matched"], 2);
    assert_eq!(summary["count"], 2);
    assert_eq!(summary["files_scanned"], 2);
    assert_eq!(summary["truncated"], false);
    assert_eq!(summary["capped"], false);
    // Every match carries the cross-reference fields.
    for r in &records {
        assert!(r["line"].as_u64().unwrap() >= 1);
        assert!(r["uuid"].is_string());
        assert!(r["msg_chars"].as_u64().unwrap() > 0);
    }
}

#[test]
fn search_no_match_returns_false() {
    let mut c = TempCorpus::new("nomatch");
    c.add_session("aaaa", "p", &[user("u1", None, "2026-01-01T00:00:00Z", "hello")]);
    let (found, records, summary) = search(&opts(&["absent_term"]), &c.files, 0);
    assert!(!found);
    assert!(records.is_empty());
    assert_eq!(summary["total_matched"], 0);
}

#[test]
fn phrases_inside_tool_results_match() {
    // Regression: tool_result content used to be searched as escaped JSON,
    // so phrases with quotes or newlines could never match.
    let mut c = TempCorpus::new("toolresult");
    c.add_session(
        "cccc",
        "p",
        &[tool_result(
            "u1",
            "2026-01-01T00:00:00Z",
            "error: cannot find value `foo_bar`\nhelp: try \"baz\" instead",
        )],
    );
    let (found, records, _) = search(&opts(&["try \"baz\" instead"]), &c.files, 0);
    assert!(found, "quoted phrase inside a tool result must match");
    assert!(records[0]["text"].as_str().unwrap().contains("try \"baz\""));
}

#[test]
fn system_records_are_searchable() {
    // Regression: system records (no `message` key) never parsed at all.
    let mut c = TempCorpus::new("sysrole");
    c.add_session(
        "dddd",
        "p",
        &[
            user("u1", None, "2026-01-01T00:00:00Z", "the user says something"),
            system("s1", "2026-01-01T00:00:01Z", "hook fired: the lint passed"),
        ],
    );
    let mut o = opts(&["the"]);
    o.role = Some("system".into());
    let (found, records, _) = search(&o, &c.files, 0);
    assert!(found);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["role"], "system");
    assert!(records[0]["text"].as_str().unwrap().contains("hook fired"));
}

// ── Caps, budgets, truncation signals ──────────────────────────────────────

#[test]
fn max_results_sets_capped_not_truncated() {
    let mut c = TempCorpus::new("capped");
    let lines: Vec<String> = (0..10)
        .map(|i| user(&format!("u{i}"), None, "2026-01-01T00:00:00Z", "needle here"))
        .collect();
    c.add_session("eeee", "p", &lines);

    let mut o = opts(&["needle"]);
    o.max_results = 3;
    let (_, records, summary) = search(&o, &c.files, 0);
    assert_eq!(records.len(), 3);
    assert_eq!(summary["total_matched"], 10);
    assert_eq!(summary["capped"], true);
    assert_eq!(summary["truncated"], false);
}

#[test]
fn token_budget_truncates_but_summary_survives() {
    let mut c = TempCorpus::new("budget");
    let long = "needle ".repeat(200);
    let lines: Vec<String> = (0..20)
        .map(|i| user(&format!("u{i}"), None, "2026-01-01T00:00:00Z", &long))
        .collect();
    c.add_session("ffff", "p", &lines);

    // Budget fits the header + a couple of records at most.
    let (_, _records, summary) = search(&opts(&["needle"]), &c.files, 300);
    assert_eq!(summary["truncated"], true, "budget cut must be flagged");
    assert_eq!(summary["total_matched"], 20, "total_matched stays exact");
}

// ── Sorting / grouping / context ───────────────────────────────────────────

#[test]
fn sort_recency_is_newest_first() {
    let mut c = TempCorpus::new("sort");
    c.add_session(
        "gggg",
        "p",
        &[
            user("u1", None, "2026-01-01T00:00:00Z", "needle old"),
            user("u2", None, "2026-03-01T00:00:00Z", "needle new"),
            user("u3", None, "2026-02-01T00:00:00Z", "needle mid"),
        ],
    );
    let mut o = opts(&["needle"]);
    o.sort = SortMode::Recency;
    let (_, records, _) = search(&o, &c.files, 0);
    let ts: Vec<&str> = records.iter().map(|r| r["timestamp"].as_str().unwrap()).collect();
    assert_eq!(ts, vec!["2026-03-01T00:00:00Z", "2026-02-01T00:00:00Z", "2026-01-01T00:00:00Z"]);
}

#[test]
fn group_by_session_collapses_hits() {
    let mut c = TempCorpus::new("group");
    c.add_session(
        "hhh1",
        "p",
        &(0..5).map(|i| user(&format!("u{i}"), None, "2026-01-01T00:00:00Z", "needle")).collect::<Vec<_>>(),
    );
    c.add_session("hhh2", "p", &[user("x", None, "2026-01-02T00:00:00Z", "needle")]);

    let mut o = opts(&["needle"]);
    o.group_by = Some(GroupMode::Session);
    o.group_samples = 2;
    let (_, records, summary) = search(&o, &c.files, 0);
    assert_eq!(records.len(), 2, "one group per session");
    assert_eq!(summary["total_matched"], 6);
    let g1 = records.iter().find(|r| r["key"] == "hhh1").unwrap();
    assert_eq!(g1["hits"], 5);
    assert_eq!(g1["samples"].as_array().unwrap().len(), 2);
}

#[test]
fn inline_context_carries_neighbors_with_lines() {
    let mut c = TempCorpus::new("ctx");
    c.add_session(
        "iii1",
        "p",
        &[
            user("u1", None, "2026-01-01T00:00:01Z", "first"),
            assistant("a1", Some("u1"), "2026-01-01T00:00:02Z", "second"),
            user("u2", Some("a1"), "2026-01-01T00:00:03Z", "the needle is here"),
            assistant("a2", Some("u2"), "2026-01-01T00:00:04Z", "fourth"),
            user("u3", Some("a2"), "2026-01-01T00:00:05Z", "fifth"),
        ],
    );
    let mut o = opts(&["needle"]);
    o.context = 2;
    let (_, records, _) = search(&o, &c.files, 0);
    assert_eq!(records.len(), 1);
    let m = &records[0];
    assert_eq!(m["line"], 3);
    let before: Vec<u64> = m["context_before"].as_array().unwrap().iter().map(|x| x["line"].as_u64().unwrap()).collect();
    let after: Vec<u64> = m["context_after"].as_array().unwrap().iter().map(|x| x["line"].as_u64().unwrap()).collect();
    assert_eq!(before, vec![1, 2]);
    assert_eq!(after, vec![4, 5]);
    assert_eq!(m["context_before"][0]["text"], "first");
    assert_eq!(m["context_after"][1]["role"], "user");
}

#[test]
fn context_ignores_filters_but_matches_respect_them() {
    let mut c = TempCorpus::new("ctxfilter");
    c.add_session(
        "jjj1",
        "p",
        &[
            assistant("a1", None, "2026-01-01T00:00:01Z", "assistant needle before"),
            user("u1", Some("a1"), "2026-01-01T00:00:02Z", "user needle match"),
            assistant("a2", Some("u1"), "2026-01-01T00:00:03Z", "assistant needle after"),
        ],
    );
    let mut o = opts(&["needle"]);
    o.role = Some("user".into());
    o.context = 1;
    let (_, records, _) = search(&o, &c.files, 0);
    assert_eq!(records.len(), 1, "role filter applies to matches");
    assert_eq!(records[0]["context_before"][0]["role"], "assistant");
    assert_eq!(records[0]["context_after"][0]["role"], "assistant");
}

// ── Filters ────────────────────────────────────────────────────────────────

#[test]
fn and_mode_requires_every_term() {
    let mut c = TempCorpus::new("andmode");
    c.add_session(
        "kkk1",
        "p",
        &[
            user("u1", None, "2026-01-01T00:00:00Z", "alpha and beta together"),
            user("u2", None, "2026-01-01T00:00:01Z", "alpha alone"),
        ],
    );
    let mut o = opts(&["alpha", "beta"]);
    o.and_mode = true;
    let (_, records, _) = search(&o, &c.files, 0);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["line"], 1);
}

#[test]
fn before_date_includes_the_whole_named_day() {
    // Regression: "--before 2026-01-05" lexically excluded every timestamp
    // ON 2026-01-05.
    let mut c = TempCorpus::new("beforeday");
    c.add_session(
        "lll1",
        "p",
        &[
            user("u1", None, "2026-01-05T23:59:00Z", "needle on the day"),
            user("u2", None, "2026-01-06T00:01:00Z", "needle after the day"),
        ],
    );
    let mut o = opts(&["needle"]);
    o.before = Some("2026-01-05".into());
    let (_, records, _) = search(&o, &c.files, 0);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["line"], 1);
}

#[test]
fn untimestamped_messages_fail_active_date_filters() {
    let mut c = TempCorpus::new("nots");
    c.add_session(
        "mmm1",
        "p",
        &[json!({"type":"user","uuid":"u1","message":{"role":"user","content":"needle no timestamp"}}).to_string()],
    );
    let mut o = opts(&["needle"]);
    o.after = Some("2026-01-01".into());
    let (found, _, _) = search(&o, &c.files, 0);
    assert!(!found, "a message that can't be dated must not pass a date filter");
    // Without date filters it still matches.
    let (found, _, _) = search(&opts(&["needle"]), &c.files, 0);
    assert!(found);
}

#[test]
fn smc_output_is_excluded_unless_asked() {
    let mut c = TempCorpus::new("smctag");
    c.add_session(
        "nnn1",
        "p",
        &[user(
            "u1",
            None,
            "2026-01-01T00:00:00Z",
            "pasted earlier smc output <smc-cc-cli> needle inside",
        )],
    );
    let (found, _, _) = search(&opts(&["needle"]), &c.files, 0);
    assert!(!found, "smc's own output must not recursively match");
    let mut o = opts(&["needle"]);
    o.include_smc = true;
    let (found, _, _) = search(&o, &c.files, 0);
    assert!(found);
}

#[test]
fn phrases_inside_tool_inputs_match() {
    // Regression: Write/Edit content and Bash commands (tool INPUT string
    // values) used to be searched as escaped JSON.
    let mut c = TempCorpus::new("toolinput");
    c.add_session(
        "uuu1",
        "p",
        &[json!({
            "type": "assistant", "uuid": "a1", "timestamp": "2026-01-01T00:00:00Z",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use", "id": "t1", "name": "Write",
                "input": {"file_path": "/tmp/cfg.yaml",
                          "content": "# ops knob, not a safety knob\nexpire_in: 86400"}
            }]}
        })
        .to_string()],
    );
    let mut o = opts(&["knob\nexpire_in: 86400"]);
    o.phrase = false;
    let (found, records, _) = search(&o, &c.files, 0);
    assert!(found, "multiline phrase inside tool input must match");
    assert!(records[0]["text"].as_str().unwrap().contains("ops knob"));
    // And --file still resolves paths from input string values.
    let mut o = opts(&["knob"]);
    o.file = Some("cfg.yaml".into());
    let (found, _, _) = search(&o, &c.files, 0);
    assert!(found);
}

#[test]
fn phrase_flag_joins_words_into_one_substring() {
    let mut c = TempCorpus::new("phrase");
    c.add_session(
        "vvv1",
        "p",
        &[
            user("u1", None, "2026-01-01T00:00:00Z", "the flux capacitor is broken again"),
            user("u2", None, "2026-01-01T00:00:01Z", "capacitor here, flux there"),
        ],
    );
    // OR mode: both messages hit.
    let (_, records, _) = search(&opts(&["flux", "capacitor"]), &c.files, 0);
    assert_eq!(records.len(), 2);
    // Phrase mode: only the exact wording.
    let mut o = opts(&["flux", "capacitor"]);
    o.phrase = true;
    let (_, records, summary) = search(&o, &c.files, 0);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["line"], 1);
    assert_eq!(summary["query"], "flux capacitor");
}

#[test]
fn zero_match_multiword_query_gets_a_hint() {
    let mut c = TempCorpus::new("hint");
    c.add_session("www1", "p", &[user("u1", None, "2026-01-01T00:00:00Z", "nothing relevant")]);
    let mut em = Emitter::capturing(0);
    cmd::search::run(&opts(&["several words not present"]), &c.files, &mut em).unwrap();
    let records = em.into_records();
    let warning = records.iter().find(|r| r["type"] == "warning");
    assert!(warning.is_some(), "0-match multi-word query must explain substring semantics");
    assert!(warning.unwrap()["message"].as_str().unwrap().contains("--phrase"));
    // No hint when terms are separate or the phrase is explicit.
    let mut em = Emitter::capturing(0);
    cmd::search::run(&opts(&["absent1", "absent2"]), &c.files, &mut em).unwrap();
    assert!(!em.into_records().iter().any(|r| r["type"] == "warning"));
}

#[test]
fn dedupe_collapses_identical_snippets() {
    let mut c = TempCorpus::new("dedupe");
    let boilerplate = "IMPORTANT: the needle instructions are repeated verbatim";
    let mut lines: Vec<String> = (0..4)
        .map(|i| user(&format!("u{i}"), None, "2026-01-01T00:00:00Z", boilerplate))
        .collect();
    lines.push(user("u9", None, "2026-01-01T00:00:01Z", "a unique needle mention"));
    c.add_session("xxx1", "p", &lines);

    let mut o = opts(&["needle"]);
    o.dedupe = true;
    let (_, records, summary) = search(&o, &c.files, 0);
    assert_eq!(records.len(), 2, "4 identical snippets collapse to 1 + 1 unique");
    assert_eq!(summary["deduped"], 3);
    assert_eq!(summary["total_matched"], 5, "raw count stays honest");
}

#[test]
fn exclude_live_skips_freshly_written_sessions() {
    let mut c = TempCorpus::new("live");
    c.add_session("yyy1", "p", &[user("u1", None, "2026-01-01T00:00:00Z", "needle live")]);
    c.add_session("yyy2", "p", &[user("u2", None, "2026-01-01T00:00:00Z", "needle old")]);
    // yyy1 was written "just now"; yyy2 an hour ago.
    c.files[0].modified = Some(std::time::SystemTime::now());
    c.files[1].modified =
        Some(std::time::SystemTime::now() - std::time::Duration::from_secs(3600));

    let mut o = opts(&["needle"]);
    o.exclude_live = Some(120);
    let (_, records, summary) = search(&o, &c.files, 0);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["session_id"], "yyy2");
    assert_eq!(summary["files_scanned"], 1);

    // Adversarial: unknown mtime counts as live (conservative).
    c.files[1].modified = None;
    let (found, _, _) = search(&o, &c.files, 0);
    assert!(!found);
}

#[test]
fn empty_thinking_blocks_are_invisible() {
    // Claude Code persists thinking signatures with EMPTY text (by design —
    // the text is cryptographically redacted). Empty blocks must not inject
    // blank lines, shift offsets, or appear in show/export output.
    let mut c = TempCorpus::new("emptythink");
    c.add_session(
        "zzt1",
        "p",
        &[json!({
            "type": "assistant", "uuid": "a1", "timestamp": "2026-01-01T00:00:00Z",
            "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "", "signature": "abc123"},
                {"type": "text", "text": "visible reply"}
            ]}
        })
        .to_string()],
    );

    // Full-content search text has no stray leading newline from the empty block.
    let (_, records, _) = search(&opts(&["visible reply"]), &c.files, 0);
    assert_eq!(records[0]["match_offset"], 0, "empty thinking must not shift offsets");

    // show omits the empty block entirely.
    let so = cmd::show::ShowOpts { from: None, to: None };
    let mut em = Emitter::capturing(0);
    cmd::show::run(&so, &c.files[0], &mut em).unwrap();
    let msg = em.into_records().into_iter().find(|r| r["type"] == "message").unwrap();
    assert!(msg.get("thinking").is_none_or(|t| t.is_null()));
}

#[test]
fn legacy_nonempty_thinking_is_searchable_and_shown() {
    // Old Claude Code versions DID persist thinking text; if present it is
    // ordinary message content — searchable and included in show.
    let mut c = TempCorpus::new("realthink");
    c.add_session(
        "zzt2",
        "p",
        &[json!({
            "type": "assistant", "uuid": "a1", "timestamp": "2026-01-01T00:00:00Z",
            "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "secretly considering the needle approach"},
                {"type": "text", "text": "public answer"}
            ]}
        })
        .to_string()],
    );
    let (found, records, _) = search(&opts(&["needle approach"]), &c.files, 0);
    assert!(found);
    assert!(records[0]["text"].as_str().unwrap().contains("considering"));

    let so = cmd::show::ShowOpts { from: None, to: None };
    let mut em = Emitter::capturing(0);
    cmd::show::run(&so, &c.files[0], &mut em).unwrap();
    let msg = em.into_records().into_iter().find(|r| r["type"] == "message").unwrap();
    assert!(msg["thinking"].as_str().unwrap().contains("secretly"));
}

// ── Adversarial corpora ────────────────────────────────────────────────────

#[test]
fn garbage_lines_never_break_the_scan() {
    let mut c = TempCorpus::new("garbage");
    c.add_session(
        "ooo1",
        "p",
        &[
            "not json at all".to_string(),
            "".to_string(),
            "{\"truncated\": ".to_string(),
            "\u{0}\u{1}\u{2}binary junk".to_string(),
            json!({"type":"user","message":"wrong shape"}).to_string(),
            json!({"type":"never_seen_before","x":1}).to_string(),
            user("u1", None, "2026-01-01T00:00:00Z", "the needle survives garbage"),
        ],
    );
    let (found, records, _) = search(&opts(&["needle"]), &c.files, 0);
    assert!(found);
    assert_eq!(records[0]["line"], 7, "line numbers stay correct past garbage");
}

#[test]
fn crlf_line_endings_parse() {
    let mut c = TempCorpus::new("crlf");
    let path = c.dir.join("crlf.jsonl");
    let content = format!(
        "{}\r\n{}\r\n",
        user("u1", None, "2026-01-01T00:00:00Z", "needle one"),
        user("u2", None, "2026-01-01T00:00:01Z", "needle two"),
    );
    std::fs::write(&path, &content).unwrap();
    c.files.push(SessionFile {
        path,
        session_id: "crlf".into(),
        project_name: "p".into(),
        size_bytes: content.len() as u64,
        modified: None,
    });
    let (_, records, _) = search(&opts(&["needle"]), &c.files, 0);
    assert_eq!(records.len(), 2);
}

#[test]
fn multibyte_snippets_center_without_panic() {
    let mut c = TempCorpus::new("multibyte");
    let text = format!("{}ネコneedle🚀{}", "🎉".repeat(400), "日本語".repeat(300));
    c.add_session("ppp1", "p", &[user("u1", None, "2026-01-01T00:00:00Z", &text)]);
    let mut o = opts(&["needle"]);
    o.snippet_len = 50;
    let (_, records, _) = search(&o, &c.files, 0);
    let snip = records[0]["text"].as_str().unwrap();
    assert!(snip.contains("needle"));
    assert!(snip.starts_with('…') && snip.ends_with('…'));
    assert_eq!(records[0]["msg_chars"].as_u64().unwrap() as usize, text.chars().count());
}

#[test]
fn empty_file_and_empty_query_behave() {
    let mut c = TempCorpus::new("empties");
    c.add_session("qqq1", "p", &[String::new()]);
    let (found, records, _) = search(&opts(&["anything"]), &c.files, 0);
    assert!(!found);
    assert!(records.is_empty());

    // Empty query list is a hard error, not a silent full dump.
    let mut em = Emitter::capturing(0);
    assert!(cmd::search::run(&opts(&[]), &c.files, &mut em).is_err());
}

// ── Other commands ─────────────────────────────────────────────────────────

#[test]
fn recent_role_filter_fills_to_limit() {
    // Regression: role filtering happened after the tail window, so a tail
    // full of assistant messages starved --role user.
    let mut c = TempCorpus::new("recentrole");
    let mut lines: Vec<String> = (0..5)
        .map(|i| user(&format!("u{i}"), None, &format!("2026-01-01T00:00:0{i}Z"), &format!("user question {i}")))
        .collect();
    lines.extend(
        (0..40).map(|i| assistant(&format!("a{i}"), None, &format!("2026-01-01T01:00:{:02}Z", i % 60), "assistant chatter")),
    );
    c.add_session("rrr1", "p", &lines);

    let o = cmd::recent::RecentOpts { limit: 3, role: Some("user".into()), project: None };
    let mut em = Emitter::capturing(0);
    let found = cmd::recent::run(&o, &c.files, &mut em).unwrap();
    assert!(found);
    let records = em.into_records();
    let recents: Vec<&Value> = records.iter().filter(|r| r["type"] == "recent").collect();
    assert_eq!(recents.len(), 3, "window must fill despite assistant-heavy tail");
    assert!(recents.iter().all(|r| r["role"] == "user"));
}

#[test]
fn sessions_reports_real_counts_and_skips_empty_previews() {
    let mut c = TempCorpus::new("sessions");
    let mut lines = vec![tool_result("t1", "2026-01-01T00:00:00Z", "tool noise first")];
    lines.extend((0..9).map(|i| {
        if i % 2 == 0 {
            assistant(&format!("a{i}"), None, &format!("2026-01-01T00:01:0{i}Z"), "reply")
        } else {
            user(&format!("u{i}"), None, &format!("2026-01-01T00:01:0{i}Z"), &format!("real question {i}"))
        }
    }));
    c.add_session("ssss1", "p", &lines);

    let o = cmd::sessions::SessionsOpts { limit: 10, project: None, after: None, before: None };
    let mut em = Emitter::capturing(0);
    let found = cmd::sessions::run(&o, &c.files, &mut em).unwrap();
    assert!(found);
    let records = em.into_records();
    let s = records.iter().find(|r| r["type"] == "session").unwrap();
    assert_eq!(s["msg_count"], 10, "must count ALL messages, not stop at 6");
    // First user record is a tool_result (no readable text) — preview must
    // come from the first user message with actual text. Note: tool_result
    // records have role user, so it's the "real question 1" line.
    assert_eq!(s["preview"], "real question 1");
    assert!(s["last_timestamp"].is_string());
}

#[test]
fn show_emits_line_and_uuid_for_cross_referencing() {
    let mut c = TempCorpus::new("show");
    c.add_session(
        "tttt1",
        "p",
        &[
            "garbage line to offset line numbers".to_string(),
            user("u1", None, "2026-01-01T00:00:00Z", "hello"),
            assistant("a1", Some("u1"), "2026-01-01T00:00:01Z", "hi"),
        ],
    );
    let o = cmd::show::ShowOpts { from: None, to: None };
    let mut em = Emitter::capturing(0);
    let found = cmd::show::run(&o, &c.files[0], &mut em).unwrap();
    assert!(found);
    let records = em.into_records();
    let msgs: Vec<&Value> = records.iter().filter(|r| r["type"] == "message").collect();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["line"], 2, "line skips the garbage line");
    assert_eq!(msgs[0]["uuid"], "u1");
    assert_eq!(msgs[1]["line"], 3);
    assert_eq!(msgs[1]["index"], 1);
}
