<div align="center">

# smc — Search My Claude

**Surgical search through Claude Code conversation logs — structured JSONL output**

[![crates.io](https://img.shields.io/crates/v/smc-cli-cc.svg?style=flat-square&color=fc8d62&logo=rust)](https://crates.io/crates/smc-cli-cc)
[![crates.io downloads](https://img.shields.io/crates/d/smc-cli-cc?style=flat-square&color=2ecc71)](https://crates.io/crates/smc-cli-cc)
[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

</div>

---

Claude Code stores every conversation as JSONL files — messages, tool calls, thinking blocks, timestamps, git context — but provides no way to search through them after context compaction. **smc** fixes that.

Every record is a JSON Line. Every command respects a token budget. Every result is parseable, composable, and consistent. Search 3GB+ of conversation history in milliseconds.

---

## Install

```bash
cargo install smc-cli-cc
```

---

## Commands

| Command | Alias | What it does |
|---------|-------|-------------|
| `smc search <query>` | `s` | Parallel full-text search across all conversations |
| `smc sessions` | `ls` | List sessions with previews, dates, and sizes |
| `smc show <id>` | — | Emit a conversation as JSONL message records |
| `smc tools <id>` | `t` | List every tool call in a session with timestamps |
| `smc stats` | — | Aggregate statistics: sessions, sizes, top projects |
| `smc export <id>` | `e` | Export a session as markdown (file or stdout) |
| `smc context <id> <line>` | `ctx` | Show messages around a specific JSONL line number |
| `smc projects` | `p` | List projects with session counts, sizes, and date ranges |
| `smc freq [mode]` | `f` | Frequency analysis: chars, words, tools, or roles |
| `smc recent` | `r` | Most recent messages across all sessions |

Session IDs support prefix matching — type just enough to be unique (e.g., `smc show 394af`).

---

## Search

The core feature. Parallel full-text search across every message, tool call, tool result, and thinking block.

```bash
smc search "authentication"                        # Basic search
smc search "bug" "error" "crash"                   # Multiple terms (OR)
smc search "bug" "deploy" -a                       # Multiple terms (AND)
smc search "refactor" --role user                  # Only user messages
smc search "deploy" -p myapp                       # Filter by project
smc search "migration" --after 2026-01-01          # After a date
smc search "hotfix" --before 2026-02-01            # Before a date
smc search "config" --tool Bash                    # Filter by tool name
smc search "merge" --branch main                   # Filter by git branch
smc search "fn\s+\w+_test" -e                      # Regex mode
smc search "todo" -n 10                            # Limit results
smc search "git push" --tool-input                 # Search tool commands/arguments only
smc search --file src/main.rs "refactor"           # Messages that touched a file
smc search "architecture" --thinking               # Search only thinking blocks
smc search "deploy" --no-thinking                  # Exclude thinking blocks
smc search "decision" --sort recency               # Newest matches first
smc search "spm" "tracker" --sort relevance        # Rank by BM25 relevance
smc search "auth" --score                          # Attach a BM25 score to each match
smc search "migration" --group-by session          # Collapse hits per session
smc search "labor" --group-by thread               # Collapse hits per conversation thread
smc search "schema" --snippet-len 200              # Wider match-centered snippets
smc search "regression" -C 2                       # Inline ±2 surrounding messages per match
smc search -F that exact thing i remember saying   # Exact-phrase mode (no shell quoting needed)
smc search "IMPORTANT:" --dedupe                   # Collapse identical snippets
smc search "cargo test" --exclude-live             # Skip the conversation running smc
```

### Search Flags

| Flag | Short | Description |
|------|-------|-------------|
| `--role <ROLE>` | | Filter by role: `user`, `assistant`, `system` |
| `--tool <TOOL>` | | Filter by tool name (substring match) |
| `--project <NAME>` | `-p` | Filter by project name (substring match) |
| `--after <DATE>` | | Only results after date (YYYY-MM-DD) |
| `--before <DATE>` | | Only results before date (YYYY-MM-DD) |
| `--branch <BRANCH>` | | Filter by git branch |
| `--and` | `-a` | Require ALL terms to match (default is OR) |
| `--regex` | `-e` | Treat query as regex |
| `--max <N>` | `-n` | Maximum results — groups in group mode (default: 50) |
| `--file <PATH>` | | Filter to messages that touch a file path |
| `--tool-input` | | Search only within tool input content |
| `--thinking` | | Search only within thinking blocks |
| `--no-thinking` | | Exclude thinking blocks from search |
| `--sort <MODE>` | | Order results: `document` (default), `recency`, `oldest`, `relevance` |
| `--score` | | Attach a BM25 relevance score to each match (implied by `--sort relevance`) |
| `--snippet-len <N>` | | Max characters per match snippet, centered on the match (default: 500) |
| `--group-by <MODE>` | | Collapse matches into groups: `session` or `thread` |
| `--group-samples <N>` | | Sample matches to include per group (default: 3) |
| `--context <N>` | `-C` | Inline N surrounding messages per match (default: 0) |
| `--phrase` | `-F` | Treat all query words as ONE exact phrase (substring match) |
| `--dedupe` | | Collapse matches with identical snippets (keeps first per sort order) |
| `--exclude-live [SECS]` | | Skip sessions written in the last SECS seconds (default 120) |
| `--include-smc` | `-i` | Include previous smc output (excluded by default) |
| `--exclude-session <ID>` | | Skip a specific session |

### Ranking, snippets & grouping

- **`--sort`** orders results. `recency`/`oldest` sort by message timestamp; `relevance` ranks by BM25 (term frequency × inverse document frequency, normalized by message length). The result cap (`--max`) is applied **after** sorting, so `--sort recency --max 10` returns the 10 *most recent* matches, not 10 arbitrary ones.
- **Snippets are centered on the match**, not truncated from the start — so the matching text is always visible. Each match also reports `match_offset` (where the hit is) and `msg_chars` (full message length).
- **`--group-by`** collapses many hits about one conversation into a single `group` record (hit count, timestamp range, and a few sample snippets). `thread` resolves each match's conversation thread by walking the `parentUuid` chain to its root, separating a long session into its distinct threads.
- **`--context N`** attaches `context_before`/`context_after` arrays to each match — up to N surrounding messages each, with `line`, `role`, `timestamp`, and a 200-char text preview. Context shows the *conversation* around a hit, so it deliberately ignores role/tool/date filters. Often replaces a follow-up `smc context` call entirely.
- **Date filters** treat a bare `--before YYYY-MM-DD` as inclusive of that whole day, and messages without timestamps are excluded whenever a date filter is active.
- **Query semantics**: separate words are OR'd (use `-a` for AND, `--sort relevance` for BM25 ranking); a single quoted multi-word argument — or `-F`/`--phrase` — matches as one exact substring. A zero-match multi-word query emits a `warning` record explaining the distinction.
- **`--dedupe`** collapses matches whose snippets are byte-identical (system-reminder boilerplate echoed into many sessions), keeping the first per sort order; the summary reports how many were dropped in `deduped` while `total_matched` stays raw.
- **Tool inputs and results are searched as text**: Write/Edit file content, Bash commands, and tool-result output are extracted from their JSON containers, so multiline and quoted phrases match naturally.

### AI-Friendly Features

smc is designed to work well when used by AI assistants inside Claude Code sessions:

```bash
smc search "bug" --exclude-session 394af           # Skip a session by ID
smc search "bug" --exclude-live                    # Skip live sessions (the one running smc)
smc search "bug" -i                                # Include previous smc output
```

`--exclude-live` solves self-matching: the conversation invoking smc logs its own commands, so a search for `"cargo publish"` finds the very command that ran it. Excluding sessions written in the last ~2 minutes (transcript writes are debounced) keeps results historical.

Every smc invocation begins with a `meta` record stamping the `<smc-cc-cli>` tag (and the tool version) into its output. By default, search **excludes** any conversation record containing that tag — so an AI searching for "X" never matches its own previous search output for "X". The header is emitted before any token-budget truncation, so the guard holds even on cut-short output. Use `-i`/`--include-smc` to opt back in.

---

## Output Format

All output is JSON Lines — one record per line, zero ANSI, zero pagination. Every stream opens with a `meta` record and search closes with a `summary`:

```jsonl
{"type":"meta","tool":"smc","tag":"<smc-cc-cli>","version":"0.9.1"}
{"type":"match","project":"myapp","session_id":"394afc...","line":42,"uuid":"a1b2...","role":"user","timestamp":"2026-02-10T15:30:00Z","matched_query":"deploy","score":3.41,"text":"…centered on the match…","match_offset":1014,"msg_chars":1293}
{"type":"summary","query":"deploy","count":2,"total_matched":2,"files_scanned":293,"truncated":false,"capped":false,"elapsed_ms":3}
```

With `--group-by`, matches are replaced by `group` records:

```jsonl
{"type":"group","group_by":"session","key":"394afc...","project":"myapp","session_id":"394afc...","hits":12,"first_ts":"2026-02-10T15:30:00Z","last_ts":"2026-02-10T16:05:00Z","samples":[{"line":42,"timestamp":"...","text":"..."}]}
```

Field notes: `score` (BM25) appears only when scoring is on; `match_offset`/`msg_chars` tell you where the hit is and how much the snippet omits; `context_before`/`context_after` appear only with `--context N`; the summary's `total_matched` is the true match count, `truncated` flags token-budget cutoff, and `capped` flags that `--max` hid more. Summaries are always emitted, even when truncated — on every command.

Exit codes are real signals: `0` = matches/results found, `1` = clean run with no results, `2` = error.

Project names are read from the `cwd` field of the session records themselves (exact, even for dash/dot-containing directory names), not from the lossy dash-encoded directory name.

Every command emits typed records with a `type` field. Pipe through `jq` for formatting:

```bash
smc search "auth" | jq 'select(.type == "match") | {project, role, line}'
smc stats | jq '.projects[] | {name, sessions}'
smc sessions -n 5 | jq 'select(.type == "session") | {session_id, project, preview}'
```

---

## Browse & Inspect

```bash
# List sessions (most recent first)
smc sessions                           # Default: 20 most recent
smc sessions -n 50                     # Show more
smc sessions -p MyProject              # Filter by project
smc sessions --after 2026-02-01        # After a date

# View a conversation
smc show 394afc                        # Emit as JSONL message records
smc show 394afc --thinking             # Include thinking blocks
smc show 394afc --from 5 --to 15       # Specific message range (by message index)

# Drill into search results
smc context 394afc 50                  # Messages around line 50
smc context 394afc 50 -C 5            # Wider context window

# See what tools were used
smc tools 394afc

# Export for sharing
smc export 394afc                      # Save as <session-id>.md
smc export 394afc --md report.md       # Custom output path
smc export 394afc -o                   # Markdown to stdout

# Recent messages
smc recent                             # Last 10 across all sessions
smc recent -p MyProject                # Filter by project
smc recent --role user                 # Only user messages
```

Records from `show`, `tools`, and `recent` carry the JSONL `line` number (and `uuid` where available), so any of them can be fed straight into `smc context <session> <line>`. `sessions` records include an exact full-scan `msg_count`, `last_timestamp`, and a `preview` taken from the first user message with readable text.

---

## Analytics

```bash
smc stats        # Total sessions, size, top projects
smc projects     # All projects with session counts and date ranges
```

### Frequency Analysis

```bash
smc freq              # Character frequency (parsed message content) — default
smc freq --raw        # Character frequency (raw JSONL bytes)
smc freq words        # Most common words
smc freq tools        # Tool usage breakdown
smc freq roles        # Message counts by role
smc freq words -n 50  # Top 50 words
```

Modes can be abbreviated: `chars`/`c`, `words`/`w`, `tools`/`t`, `roles`/`r`.

---

## Global Options

```bash
--path <PATH>        # Override Claude projects directory (default: ~/.claude/projects)
--max-tokens <N>     # Hard cap on output tokens (0 = unlimited)
```

---

## Library Usage

smc is also a Rust library crate. Add it to your project:

```bash
cargo add smc-cli-cc
```

```rust
use smc::{cmd, output::Emitter, util::discover};

// Discover all conversation files
let dir = discover::claude_dir(None)?;
let files = discover::discover_jsonl_files(&dir)?;

// Search programmatically
let opts = cmd::search::SearchOpts {
    queries: vec!["authentication".into()],
    max_results: 10,
    // ...all other fields
};

// Emit to stdout
let mut em = Emitter::stdout(0);
cmd::search::run(&opts, &files, &mut em)?;

// Or capture in memory (for tests / programmatic use)
let mut em = Emitter::capturing(0);
cmd::search::run(&opts, &files, &mut em)?;
let records = em.into_records(); // Vec<serde_json::Value>
```

Available modules: `cmd` (search, sessions, show, tools, export, context, stats, projects, freq, recent), `models`, `output`, `util`.

---

## How It Works

Claude Code stores conversation logs as JSONL files in `~/.claude/projects/`. Each project gets a directory, and each session is a `.jsonl` file containing one JSON record per line:

| Record Type | Contents |
|-------------|----------|
| `user` | Your messages |
| `assistant` | Claude's responses — text, thinking blocks, tool calls |
| `system` | System prompts and context |
| `file-history-snapshot` | File state snapshots |
| `progress` | Progress indicators |

smc uses [Rayon](https://github.com/rayon-rs/rayon) for parallel file processing — all CPU cores scan simultaneously, which is why it searches gigabytes in milliseconds.

---

## Development

Requires Rust 1.85+ (edition 2024).

```bash
git clone https://github.com/scalecode-solutions/smc-cli-cc.git
cd smc-cli-cc
cargo build --release
cargo install --path .
```

### Version Management

```bash
make patch    # 0.8.0 → 0.8.1
make minor    # 0.8.0 → 0.9.0
make major    # 0.8.0 → 1.0.0
make current  # Show current version
```

---

## Why?

Claude Code logs everything — every message, tool call, thinking block, timestamp, git branch — as structured JSONL. But after context compaction, that history is gone. The only way to recover it was manually grepping through files that can be hundreds of megabytes.

smc gives Claude (and you) instant access to all of it — as machine-parseable JSONL, composable with `jq`, respecting token budgets, and designed to sit naturally alongside tools like [mvtk](https://crates.io/crates/mvtk).

---

## License

MIT
