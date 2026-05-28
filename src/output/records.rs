/// Shared record types emitted by all subcommands.
use serde::Serialize;

/// Marker stamped into every smc output stream (via the leading `meta` record /
/// markdown comment). `search` excludes any conversation record whose text
/// contains this tag, so smc's own output never recursively matches itself.
pub const SMC_TAG: &str = "<smc-cc-cli>";

// ── Meta (output header) ─────────────────────────────────────────────────────

/// First record of every JSON output stream: provenance + the anti-recursion tag.
#[derive(Serialize, Debug)]
pub struct MetaRecord {
    #[serde(rename = "type")]
    pub record_type: &'static str,
    pub tool: &'static str,
    pub tag: &'static str,
    pub version: &'static str,
}

impl MetaRecord {
    pub fn current() -> Self {
        Self {
            record_type: "meta",
            tool: "smc",
            tag: SMC_TAG,
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

// ── Error / Warning ────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
pub struct ErrorRecord {
    #[serde(rename = "type")]
    pub record_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub message: String,
}

impl ErrorRecord {
    pub fn new(file: Option<impl Into<String>>, message: impl Into<String>) -> Self {
        Self { record_type: "error", file: file.map(Into::into), message: message.into() }
    }

    pub fn warn(file: Option<impl Into<String>>, message: impl Into<String>) -> Self {
        Self { record_type: "warning", file: file.map(Into::into), message: message.into() }
    }
}

// ── Summary ────────────────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
pub struct SummaryRecord {
    #[serde(rename = "type")]
    pub record_type: &'static str,
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_scanned: Option<usize>,
    pub elapsed_ms: u128,
}
