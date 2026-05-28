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
    pub message: Message,
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

// ── Content extraction ─────────────────────────────────────────────────────

impl MessageRecord {
    /// `parentUuid` as a string (it's a string or null in the logs).
    pub fn parent_uuid_str(&self) -> Option<String> {
        self.parent_uuid.as_ref().and_then(|v| v.as_str().map(String::from))
    }

    /// All text content (text blocks + thinking + tool use/results).
    pub fn text_content(&self) -> String {
        match &self.message.content {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => parts.push(text.as_str()),
                        ContentBlock::Thinking { thinking } => parts.push(thinking.as_str()),
                        ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => {}
                        ContentBlock::Other => {}
                    }
                }
                parts.join("\n")
            }
        }
    }

    /// Text content excluding thinking blocks.
    pub fn text_no_thinking(&self) -> String {
        match &self.message.content {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    if let ContentBlock::Text { text } = block {
                        parts.push(text.as_str());
                    }
                }
                parts.join("\n")
            }
        }
    }

    /// Only thinking block content.
    pub fn thinking_content(&self) -> String {
        match &self.message.content {
            MessageContent::Blocks(blocks) => {
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

    /// Only tool input content (name + serialized input).
    pub fn tool_input_content(&self) -> String {
        match &self.message.content {
            MessageContent::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    if let ContentBlock::ToolUse { name, input, .. } = block {
                        parts.push(format!("[{}] {}", name, input));
                    }
                }
                parts.join("\n")
            }
            _ => String::new(),
        }
    }

    /// Names of tools called in this message.
    pub fn tool_names(&self) -> Vec<&str> {
        match &self.message.content {
            MessageContent::Blocks(blocks) => blocks
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
        match &self.message.content {
            MessageContent::Blocks(blocks) => blocks.iter().any(|block| match block {
                ContentBlock::ToolUse { input, .. } => {
                    input.to_string().to_lowercase().contains(&path_lower)
                }
                ContentBlock::ToolResult { content: Some(c), .. } => {
                    c.to_string().to_lowercase().contains(&path_lower)
                }
                _ => false,
            }),
            _ => false,
        }
    }

    /// Full content including tool calls/results (for search).
    pub fn full_content(&self) -> String {
        match &self.message.content {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => {
                let mut parts = Vec::new();
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => parts.push(text.clone()),
                        ContentBlock::Thinking { thinking } => parts.push(thinking.clone()),
                        ContentBlock::ToolUse { name, input, .. } => {
                            parts.push(format!("[tool: {}] {}", name, input));
                        }
                        ContentBlock::ToolResult { content: Some(c), .. } => {
                            parts.push(format!("[result] {}", c));
                        }
                        _ => {}
                    }
                }
                parts.join("\n")
            }
        }
    }
}
