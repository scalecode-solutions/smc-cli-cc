/// Claude Code JSONL record types — deserialization only.
///
/// Claude Code stores conversations as JSONL in ~/.claude/projects/.
/// Each line is one of these record types.
use serde::Deserialize;

// ── Top-level record ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Record {
    User(MessageRecord),
    Assistant(MessageRecord),
    System(MessageRecord),
    FileHistorySnapshot(serde_json::Value),
    Progress(serde_json::Value),
    #[serde(other)]
    Unknown,
}

impl Record {
    pub fn as_message(&self) -> Option<&MessageRecord> {
        match self {
            Record::User(r) | Record::Assistant(r) | Record::System(r) => Some(r),
            _ => None,
        }
    }

    pub fn role(&self) -> &'static str {
        match self {
            Record::User(_) => "user",
            Record::Assistant(_) => "assistant",
            Record::System(_) => "system",
            _ => "other",
        }
    }

    pub fn is_message(&self) -> bool {
        matches!(self, Record::User(_) | Record::Assistant(_) | Record::System(_))
    }
}

// ── Message ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRecord {
    pub uuid: Option<String>,
    pub parent_uuid: Option<serde_json::Value>,
    pub session_id: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    /// User/assistant records carry the message here.
    pub message: Option<Message>,
    /// System records have no `message` — their text lives in a top-level
    /// `content` field instead (and some, like turn-duration markers, have
    /// no text at all).
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: MessageContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Thinking { thinking: String },
    ToolUse {
        id: Option<String>,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: Option<String>,
        content: Option<serde_json::Value>,
    },
    #[serde(other)]
    Other,
}

// ── Content views ──────────────────────────────────────────────────────────

/// Borrowed view over a record's content, regardless of where it lives
/// (`message.content` for user/assistant, top-level `content` for system).
pub enum ContentView<'a> {
    Text(&'a str),
    Blocks(&'a [ContentBlock]),
    None,
}

/// Collect every string leaf in a JSON value (depth-first), joined by
/// newlines. Used to make tool-use *inputs* searchable as text: a Write call's
/// file content or a Bash command lives inside JSON string values, and
/// serializing the whole object (the old behavior) JSON-escaped quotes and
/// newlines so multiline/quoted phrases could never match.
pub fn json_string_values(v: &serde_json::Value) -> String {
    fn walk<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match v {
            serde_json::Value::String(s) => out.push(s.as_str()),
            serde_json::Value::Array(items) => {
                for it in items {
                    walk(it, out);
                }
            }
            serde_json::Value::Object(o) => {
                for val in o.values() {
                    walk(val, out);
                }
            }
            _ => {}
        }
    }
    let mut parts = Vec::new();
    walk(v, &mut parts);
    parts.join("\n")
}

/// Extract the human text from a tool_result `content` value. On disk it is
/// either a plain string or a list of `{type, text}` blocks — serializing the
/// whole Value (the old behavior) produced JSON-escaped text (`\"`, `\n`, block
/// wrappers), which silently broke phrase matching inside tool results.
pub fn tool_result_text(c: &serde_json::Value) -> String {
    match c {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            let mut parts: Vec<&str> = Vec::new();
            for it in items {
                match it {
                    serde_json::Value::String(s) => parts.push(s.as_str()),
                    serde_json::Value::Object(o) => {
                        if let Some(t) = o.get("text").and_then(|t| t.as_str()) {
                            parts.push(t);
                        }
                    }
                    _ => {}
                }
            }
            parts.join("\n")
        }
        other => other.to_string(),
    }
}

// ── Content extraction ─────────────────────────────────────────────────────

impl MessageRecord {
    /// `parentUuid` as a string (it's a string or null in the logs).
    pub fn parent_uuid_str(&self) -> Option<String> {
        self.parent_uuid.as_ref().and_then(|v| v.as_str().map(String::from))
    }

    /// Unified view over the record's content wherever it lives.
    pub fn content_view(&self) -> ContentView<'_> {
        if let Some(m) = &self.message {
            match &m.content {
                MessageContent::Text(s) => ContentView::Text(s),
                MessageContent::Blocks(b) => ContentView::Blocks(b),
            }
        } else if let Some(c) = &self.content {
            ContentView::Text(c)
        } else {
            ContentView::None
        }
    }

    /// All text content (text blocks + thinking; tool use/results excluded).
    pub fn text_content(&self) -> String {
        match self.content_view() {
            ContentView::Text(s) => s.to_string(),
            ContentView::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => parts.push(text.as_str()),
                        ContentBlock::Thinking { thinking } => parts.push(thinking.as_str()),
                        _ => {}
                    }
                }
                parts.join("\n")
            }
            ContentView::None => String::new(),
        }
    }

    /// Text content excluding thinking blocks.
    pub fn text_no_thinking(&self) -> String {
        match self.content_view() {
            ContentView::Text(s) => s.to_string(),
            ContentView::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    if let ContentBlock::Text { text } = block {
                        parts.push(text.as_str());
                    }
                }
                parts.join("\n")
            }
            ContentView::None => String::new(),
        }
    }

    /// Only thinking block content.
    pub fn thinking_content(&self) -> String {
        match self.content_view() {
            ContentView::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    if let ContentBlock::Thinking { thinking } = block {
                        parts.push(thinking.as_str());
                    }
                }
                parts.join("\n")
            }
            _ => String::new(),
        }
    }

    /// Only tool input content (name + the input's string values as text).
    pub fn tool_input_content(&self) -> String {
        match self.content_view() {
            ContentView::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    if let ContentBlock::ToolUse { name, input, .. } = block {
                        parts.push(format!("[{}] {}", name, json_string_values(input)));
                    }
                }
                parts.join("\n")
            }
            _ => String::new(),
        }
    }

    /// Names of tools called in this message.
    pub fn tool_names(&self) -> Vec<&str> {
        match self.content_view() {
            ContentView::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }

    /// Check if any tool input/result references a file path (substring match).
    pub fn touches_file(&self, path: &str) -> bool {
        let path_lower = path.to_lowercase();
        match self.content_view() {
            ContentView::Blocks(blocks) => blocks.iter().any(|block| match block {
                ContentBlock::ToolUse { input, .. } => {
                    // Paths always live inside string values.
                    json_string_values(input).to_lowercase().contains(&path_lower)
                }
                ContentBlock::ToolResult { content: Some(c), .. } => {
                    tool_result_text(c).to_lowercase().contains(&path_lower)
                }
                _ => false,
            }),
            _ => false,
        }
    }

    /// Full content including tool calls/results (for search).
    pub fn full_content(&self) -> String {
        match self.content_view() {
            ContentView::Text(s) => s.to_string(),
            ContentView::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => parts.push(text.clone()),
                        ContentBlock::Thinking { thinking } => parts.push(thinking.clone()),
                        ContentBlock::ToolUse { name, input, .. } => {
                            parts.push(format!("[tool: {}] {}", name, json_string_values(input)));
                        }
                        ContentBlock::ToolResult { content: Some(c), .. } => {
                            parts.push(format!("[result] {}", tool_result_text(c)));
                        }
                        _ => {}
                    }
                }
                parts.join("\n")
            }
            ContentView::None => String::new(),
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Record {
        serde_json::from_str::<Record>(line).expect("record should parse")
    }

    #[test]
    fn system_record_without_message_parses() {
        // Real shape: system records have no `message` key at all.
        let r = parse(
            r#"{"type":"system","subtype":"turn_duration","uuid":"u1","timestamp":"2026-07-12T00:00:00Z","durationMs":1234}"#,
        );
        assert_eq!(r.role(), "system");
        let msg = r.as_message().unwrap();
        assert_eq!(msg.text_content(), "");
        assert_eq!(msg.full_content(), "");
    }

    #[test]
    fn system_record_with_top_level_content() {
        let r = parse(
            r#"{"type":"system","content":"Hook output: lint passed","uuid":"u2","timestamp":"2026-07-12T00:00:00Z"}"#,
        );
        assert_eq!(r.role(), "system");
        let msg = r.as_message().unwrap();
        assert_eq!(msg.text_content(), "Hook output: lint passed");
        assert_eq!(msg.full_content(), "Hook output: lint passed");
    }

    #[test]
    fn tool_result_text_from_block_list() {
        // The common on-disk shape: a list of {type, text} blocks.
        let v = serde_json::json!([{"type":"text","text":"line one\nline \"two\""}]);
        assert_eq!(tool_result_text(&v), "line one\nline \"two\"");
    }

    #[test]
    fn tool_result_text_from_plain_string() {
        let v = serde_json::json!("plain result");
        assert_eq!(tool_result_text(&v), "plain result");
    }

    #[test]
    fn tool_result_text_adversarial_shapes() {
        // Mixed/malformed lists must not panic and should keep what's usable.
        let v = serde_json::json!(["bare string", {"no_text": 1}, {"text": "block"}, 42, null]);
        assert_eq!(tool_result_text(&v), "bare string\nblock");
        // Non-string/list falls back to JSON serialization.
        let v = serde_json::json!({"weird": true});
        assert_eq!(tool_result_text(&v), r#"{"weird":true}"#);
    }

    #[test]
    fn full_content_searchable_across_tool_result_lines() {
        // Regression: phrases with quotes/newlines inside tool results must match.
        let r = parse(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"error: cannot find value `foo_bar`\nhelp: try \"baz\""}]}]}}"#,
        );
        let text = r.as_message().unwrap().full_content();
        assert!(text.contains("cannot find value `foo_bar`"));
        assert!(text.contains("try \"baz\""), "quotes must not be JSON-escaped");
    }

    #[test]
    fn unknown_block_type_is_tolerated() {
        let r = parse(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"banana","peel":true},{"type":"text","text":"hi"}]}}"#,
        );
        assert_eq!(r.as_message().unwrap().text_content(), "hi");
    }

    #[test]
    fn tool_input_text_is_searchable_unescaped() {
        // Regression: Write/Edit file content and Bash commands live in tool
        // INPUT string values; they used to be searched as escaped JSON.
        let r = parse(
            r##"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"/tmp/x.yaml","content":"# no app change is required.\nexpire_in: 86400\nname: \"prod\""}}]}}"##,
        );
        let msg = r.as_message().unwrap();
        let full = msg.full_content();
        assert!(full.contains("[tool: Write]"));
        assert!(full.contains("# no app change is required.\nexpire_in: 86400"));
        assert!(full.contains("name: \"prod\""), "quotes must not be JSON-escaped");
        assert!(msg.tool_input_content().contains("expire_in: 86400"));
    }

    #[test]
    fn json_string_values_walks_nested_shapes() {
        let v = serde_json::json!({
            "a": "top",
            "b": {"c": ["deep", {"d": "deeper"}], "n": 42, "t": true},
            "z": null
        });
        let s = json_string_values(&v);
        for expect in ["top", "deep", "deeper"] {
            assert!(s.contains(expect));
        }
        assert!(!s.contains("42"), "non-string leaves are not text");
        assert_eq!(json_string_values(&serde_json::json!({})), "");
    }

    #[test]
    fn touches_file_via_tool_result() {
        let r = parse(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"edited /Users/x/src/Main.swift"}]}]}}"#,
        );
        assert!(r.as_message().unwrap().touches_file("main.swift"));
        assert!(!r.as_message().unwrap().touches_file("other.rs"));
    }
}
