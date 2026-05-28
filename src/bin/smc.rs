/// smc — Search My Claude.
///
/// Clap CLI harness. All business logic lives in smc::cmd::*.
use clap::{Parser, Subcommand};
use smc::cmd;
use smc::output::Emitter;
use smc::util::discover;

// ── Top-level ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "smc",
    version,
    about = "Surgical search through Claude Code conversation logs",
    long_about = "Structured JSONL output for search, sessions, show, tools, export, \
                  context, stats, projects, freq, recent. Every record is machine-parseable \
                  JSON Lines — zero ANSI, zero pagination.",
    after_help = "Exit codes: 0 = success/match, 1 = no results, 2 = error\n\n\
                  NOTE: All output is single-line JSONL. Pipe through `cat` or redirect \
                  to a file to get clean output."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Path to Claude projects directory (default: ~/.claude/projects)
    #[arg(long, global = true)]
    path: Option<String>,

    /// Hard cap on output tokens (0 = unlimited)
    #[arg(long, global = true, value_name = "N")]
    max_tokens: Option<usize>,
}

// ── Commands ───────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum Commands {
    /// Parallel full-text search across all conversations
    #[command(visible_alias = "s")]
    Search(SearchArgs),

    /// List sessions with previews, dates, and sizes
    #[command(visible_alias = "ls")]
    Sessions(SessionsArgs),

    /// Pretty-print a conversation as JSONL message records
    Show(ShowArgs),

    /// List every tool call in a session with timestamps
    #[command(visible_alias = "t")]
    Tools(ToolsArgs),

    /// Aggregate statistics: sessions, sizes, top projects
    Stats,

    /// Export a session as markdown (file or stdout)
    #[command(visible_alias = "e")]
    Export(ExportArgs),

    /// Show messages around a specific JSONL line number
    #[command(visible_alias = "ctx")]
    Context(ContextArgs),

    /// List projects with session counts, sizes, and date ranges
    #[command(visible_alias = "p")]
    Projects,

    /// Frequency analysis: chars, words, tools, or roles
    #[command(visible_alias = "f")]
    Freq(FreqArgs),

    /// Most recent messages across all sessions
    #[command(visible_alias = "r")]
    Recent(RecentArgs),
}

// ── search ─────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    about = "Parallel full-text search across all conversations",
    long_about = "Parallel full-text search across every message, tool call, tool result, \
                  and thinking block in your Claude Code conversation logs. Supports \
                  multi-term OR/AND, regex, role/tool/project/date/branch filters, \
                  file-path matching, and thinking-block isolation."
)]
struct SearchArgs {
    /// Search queries (multiple terms are OR'd together)
    query: Vec<String>,

    /// Treat query as regex
    #[arg(long, short = 'e')]
    regex: bool,

    /// Require ALL terms to match (default is OR)
    #[arg(long, short = 'a')]
    and: bool,

    /// Filter by role (user, assistant, system)
    #[arg(long)]
    role: Option<String>,

    /// Filter by tool name
    #[arg(long)]
    tool: Option<String>,

    /// Filter by project name (substring match)
    #[arg(long, short)]
    project: Option<String>,

    /// Only results after this date (YYYY-MM-DD)
    #[arg(long)]
    after: Option<String>,

    /// Only results before this date (YYYY-MM-DD)
    #[arg(long)]
    before: Option<String>,

    /// Filter by git branch
    #[arg(long)]
    branch: Option<String>,

    /// Maximum number of results
    #[arg(long, short = 'n', default_value = "50")]
    max: usize,

    /// Include results from previous smc output (excluded by default)
    #[arg(long, short = 'i')]
    include_smc: bool,

    /// Exclude a specific session ID
    #[arg(long)]
    exclude_session: Option<String>,

    /// Filter to messages that touch a file path
    #[arg(long)]
    file: Option<String>,

    /// Search only within tool input content
    #[arg(long)]
    tool_input: bool,

    /// Search only within thinking blocks
    #[arg(long)]
    thinking: bool,

    /// Exclude thinking blocks from search
    #[arg(long)]
    no_thinking: bool,

    /// Max characters per match snippet, centered on the match
    #[arg(long, default_value = "500")]
    snippet_len: usize,

    /// Result ordering: document (default), recency (newest first), or oldest
    #[arg(long, value_enum, default_value_t = cmd::search::SortMode::Document)]
    sort: cmd::search::SortMode,

    /// Collapse matches into groups: session or thread
    #[arg(long, value_enum)]
    group_by: Option<cmd::search::GroupMode>,

    /// Sample matches to include per group (with --group-by)
    #[arg(long, default_value = "3")]
    group_samples: usize,
}

// ── sessions ───────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    about = "List sessions with previews, dates, and sizes",
    long_about = "List conversation sessions sorted by date. Each record includes the \
                  session ID, project name, file size, first timestamp, first user \
                  message preview, and message count."
)]
struct SessionsArgs {
    /// Maximum sessions to show
    #[arg(long, short = 'n', default_value = "20")]
    limit: usize,

    /// Filter by project name
    #[arg(long, short)]
    project: Option<String>,

    /// Only sessions after this date (YYYY-MM-DD)
    #[arg(long)]
    after: Option<String>,

    /// Only sessions before this date (YYYY-MM-DD)
    #[arg(long)]
    before: Option<String>,
}

// ── show ───────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    about = "Pretty-print a conversation as JSONL message records",
    long_about = "Emit every message in a session as structured JSONL. Each record \
                  includes role, timestamp, text content, and tool calls. Use --thinking \
                  to include thinking blocks, --from/--to to slice by message index."
)]
struct ShowArgs {
    /// Session ID (or prefix)
    session: String,

    /// Include thinking blocks
    #[arg(long)]
    thinking: bool,

    /// Start from this message number
    #[arg(long)]
    from: Option<usize>,

    /// End at this message number
    #[arg(long)]
    to: Option<usize>,
}

// ── tools ──────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    about = "List every tool call in a session with timestamps",
    long_about = "Emit one record per tool invocation in a session — tool name, \
                  timestamp, role, and a preview of the input arguments."
)]
struct ToolsArgs {
    /// Session ID (or prefix)
    session: String,
}

// ── export ─────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    about = "Export a session as markdown (file or stdout)",
    long_about = "Convert a full conversation session to readable markdown with \
                  role headers, timestamps, tool call blocks, and thinking details. \
                  Writes to a file by default or streams to stdout with --output."
)]
struct ExportArgs {
    /// Session ID (or prefix)
    session: String,

    /// Print markdown to stdout
    #[arg(long, short)]
    output: bool,

    /// Output file path (default: <session-id>.md)
    #[arg(long, value_name = "FILE")]
    md: Option<String>,
}

// ── context ────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    about = "Show messages around a specific JSONL line number",
    long_about = "Given a line number from a search result, show the surrounding \
                  messages for context. Each record is tagged with is_target to \
                  identify the focal message."
)]
struct ContextArgs {
    /// Session ID (or prefix)
    session: String,

    /// Line number to center on
    line: usize,

    /// Number of messages to show before and after
    #[arg(long, short = 'C', default_value = "3")]
    context: usize,
}

// ── freq ───────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    about = "Frequency analysis: chars, words, tools, or roles",
    long_about = "Count character distributions, word frequencies, tool usage, \
                  or message role breakdowns across all conversation logs. \
                  Modes: chars (c), words (w), tools (t), roles (r). \
                  Use --raw with chars mode to count raw JSONL bytes."
)]
struct FreqArgs {
    /// What to count: chars, words, tools, roles
    #[arg(default_value = "chars")]
    mode: String,

    /// Max items to show (for words mode)
    #[arg(long, short = 'n', default_value = "30")]
    limit: usize,

    /// Count raw file bytes instead of parsed message content
    #[arg(long)]
    raw: bool,
}

// ── recent ─────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    about = "Most recent messages across all sessions",
    long_about = "Show the latest messages across all sessions, sorted by timestamp. \
                  Filter by role or project. Useful for picking up where you left off."
)]
struct RecentArgs {
    /// Number of recent messages to show
    #[arg(long, short = 'n', default_value = "10")]
    limit: usize,

    /// Filter by role
    #[arg(long)]
    role: Option<String>,

    /// Filter by project name (substring match)
    #[arg(long, short)]
    project: Option<String>,
}

// ── main ───────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let max_tokens = cli.max_tokens.unwrap_or(0);

    let result = run(cli, max_tokens);

    match result {
        Ok(true) => std::process::exit(0),
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("{:#}", e);
            std::process::exit(2);
        }
    }
}

/// Returns Ok(true) for success/matches, Ok(false) for no results.
fn run(cli: Cli, max_tokens: usize) -> anyhow::Result<bool> {
    let claude_dir = discover::claude_dir(cli.path.as_deref())?;
    let files = discover::discover_jsonl_files(&claude_dir)?;

    match cli.command {
        Commands::Search(args) => {
            let opts = cmd::search::SearchOpts {
                queries: args.query,
                is_regex: args.regex,
                and_mode: args.and,
                role: args.role,
                tool: args.tool,
                project: args.project,
                after: args.after,
                before: args.before,
                branch: args.branch,
                file: args.file,
                tool_input: args.tool_input,
                thinking_only: args.thinking,
                no_thinking: args.no_thinking,
                max_results: args.max,
                include_smc: args.include_smc,
                exclude_session: args.exclude_session,
                max_tokens,
                snippet_len: args.snippet_len,
                sort: args.sort,
                group_by: args.group_by,
                group_samples: args.group_samples,
            };
            let mut em = Emitter::stdout(max_tokens);
            cmd::search::run(&opts, &files, &mut em)?;
        }

        Commands::Sessions(args) => {
            let opts = cmd::sessions::SessionsOpts {
                limit: args.limit,
                project: args.project,
                after: args.after,
                before: args.before,
            };
            let mut em = Emitter::stdout(max_tokens);
            cmd::sessions::run(&opts, &files, &mut em)?;
        }

        Commands::Show(args) => {
            let file = discover::find_session(&files, &args.session)?;
            let opts = cmd::show::ShowOpts {
                session: args.session,
                thinking: args.thinking,
                from: args.from,
                to: args.to,
                max_tokens,
            };
            let mut em = Emitter::stdout(max_tokens);
            cmd::show::run(&opts, file, &mut em)?;
        }

        Commands::Tools(args) => {
            let file = discover::find_session(&files, &args.session)?;
            let opts = cmd::tools::ToolsOpts {
                session: args.session,
                max_tokens,
            };
            let mut em = Emitter::stdout(max_tokens);
            cmd::tools::run(&opts, file, &mut em)?;
        }

        Commands::Stats => {
            let opts = cmd::stats::StatsOpts { max_tokens };
            let mut em = Emitter::stdout(max_tokens);
            cmd::stats::run(&opts, &files, &mut em)?;
        }

        Commands::Export(args) => {
            let file = discover::find_session(&files, &args.session)?;
            let opts = cmd::export::ExportOpts {
                session: args.session,
                to_stdout: args.output,
                md_path: args.md,
            };
            let mut em = Emitter::stdout(max_tokens);
            cmd::export::run(&opts, file, &mut em)?;
        }

        Commands::Context(args) => {
            let file = discover::find_session(&files, &args.session)?;
            let opts = cmd::context::ContextOpts {
                session: args.session,
                line: args.line,
                context: args.context,
                max_tokens,
            };
            let mut em = Emitter::stdout(max_tokens);
            cmd::context::run(&opts, file, &mut em)?;
        }

        Commands::Projects => {
            let opts = cmd::projects::ProjectsOpts { max_tokens };
            let mut em = Emitter::stdout(max_tokens);
            cmd::projects::run(&opts, &files, &mut em)?;
        }

        Commands::Freq(args) => {
            let mode = cmd::freq::FreqMode::parse(&args.mode)?;
            let opts = cmd::freq::FreqOpts {
                mode,
                limit: args.limit,
                raw: args.raw,
                max_tokens,
            };
            let mut em = Emitter::stdout(max_tokens);
            cmd::freq::run(&opts, &files, &mut em)?;
        }

        Commands::Recent(args) => {
            let opts = cmd::recent::RecentOpts {
                limit: args.limit,
                role: args.role,
                project: args.project,
                max_tokens,
            };
            let mut em = Emitter::stdout(max_tokens);
            cmd::recent::run(&opts, &files, &mut em)?;
        }
    }

    Ok(true)
}
