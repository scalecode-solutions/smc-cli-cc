/// Emitter<W> — the single output channel for all subcommands.
///
/// # Contract
/// - Every record is emitted as one JSON line (JSONL).
/// - Token budget is tracked; once exhausted `emit()` returns `false`
///   and sets `self.truncated = true`.  Callers should break their loop.
/// - Warnings are emitted inline as `{"type":"warning",...}` records —
///   never to stderr.
/// - `flush()` must be called by the caller before process exit.
use std::io::{BufWriter, Write};

use anyhow::Result;
use serde::Serialize;

use super::records::{ErrorRecord, MetaRecord, SMC_TAG};
use crate::util::tokens;

pub struct Emitter<W: Write> {
    out: BufWriter<W>,
    /// Maximum token budget (0 = unlimited).
    budget: usize,
    /// Tokens consumed so far.
    used: usize,
    /// Set when the budget was exhausted and output was cut short.
    pub truncated: bool,
    /// Whether the leading provenance/anti-recursion header has been written.
    header_written: bool,
}

impl<W: Write> Emitter<W> {
    pub fn new(writer: W, budget: usize) -> Self {
        Self { out: BufWriter::new(writer), budget, used: 0, truncated: false, header_written: false }
    }

    /// Write the one-time leading header that stamps the anti-recursion tag into
    /// the stream. Written unconditionally (never blocked by the budget) so it
    /// always reaches the output even when results are later truncated — that's
    /// what lets `search` reliably exclude smc's own output. `markdown` chooses a
    /// comment form for raw/markdown streams instead of a JSON `meta` record.
    fn ensure_header(&mut self, markdown: bool) -> Result<()> {
        if self.header_written {
            return Ok(());
        }
        self.header_written = true;
        let line = if markdown {
            format!("<!-- smc {} v{} -->", SMC_TAG, env!("CARGO_PKG_VERSION"))
        } else {
            serde_json::to_string(&MetaRecord::current())?
        };
        self.used += tokens::approx_line(line.len());
        self.out.write_all(line.as_bytes())?;
        self.out.write_all(b"\n")?;
        Ok(())
    }

    /// Serialize `rec` as a JSON line and write it.
    /// Returns `Ok(true)` on success, `Ok(false)` when the token budget
    /// has been exhausted (the record was NOT written; caller should stop).
    pub fn emit<T: Serialize>(&mut self, rec: &T) -> Result<bool> {
        self.ensure_header(false)?;
        let json = serde_json::to_string(rec)?;
        let cost = tokens::approx_line(json.len());
        if self.budget > 0 && self.used + cost > self.budget {
            self.truncated = true;
            return Ok(false);
        }
        self.out.write_all(json.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.used += cost;
        Ok(true)
    }

    /// Emit a record unconditionally, ignoring the token budget.
    /// For small, critical trailers (e.g. a search `summary`) that must always
    /// reach the consumer — otherwise budget truncation would silently drop the
    /// one record that signals the output was incomplete.
    pub fn emit_always<T: Serialize>(&mut self, rec: &T) -> Result<()> {
        self.ensure_header(false)?;
        let json = serde_json::to_string(rec)?;
        self.used += tokens::approx_line(json.len());
        self.out.write_all(json.as_bytes())?;
        self.out.write_all(b"\n")?;
        Ok(())
    }

    /// Emit a `{"type":"warning",...}` record inline.
    /// Never returns an error — file-level warnings must never abort the run.
    pub fn warn(&mut self, file: Option<&str>, msg: &str) {
        let rec = ErrorRecord::warn(file, msg);
        let _ = self.emit(&rec);
    }

    /// Flush the underlying writer.
    pub fn flush(&mut self) -> Result<()> {
        self.out.flush().map_err(Into::into)
    }

    /// Emit a raw text line (not JSON-serialized).
    /// Useful for markdown output modes.
    /// Obeys the token budget the same way `emit()` does.
    pub fn raw(&mut self, line: &str) -> Result<bool> {
        self.ensure_header(true)?;
        let cost = tokens::approx_line(line.len());
        if self.budget > 0 && self.used + cost > self.budget {
            self.truncated = true;
            return Ok(false);
        }
        self.out.write_all(line.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.used += cost;
        Ok(true)
    }

    /// How many tokens have been emitted so far.
    pub fn tokens_used(&self) -> usize { self.used }
}

// ── Convenience constructors ───────────────────────────────────────────────

impl Emitter<std::io::Stdout> {
    pub fn stdout(budget: usize) -> Self {
        Self::new(std::io::stdout(), budget)
    }
}

impl Emitter<Vec<u8>> {
    pub fn capturing(budget: usize) -> Self {
        Self::new(Vec::new(), budget)
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.out.flush().expect("flush of Vec<u8> cannot fail");
        self.out.into_inner().expect("BufWriter<Vec<u8>> always succeeds")
    }

    pub fn into_records(self) -> Vec<serde_json::Value> {
        let bytes = self.into_bytes();
        bytes
            .split(|&b| b == b'\n')
            .filter(|s| !s.is_empty())
            .filter_map(|s| serde_json::from_slice(s).ok())
            .collect()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn emits_jsonl() {
        let mut em = Emitter::capturing(0);
        em.emit(&json!({"type": "match", "line": 1})).unwrap();
        em.emit(&json!({"type": "match", "line": 2})).unwrap();
        let records = em.into_records();
        // Leading meta header + the two emitted records.
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["type"], "meta");
        assert_eq!(records[1]["line"], 1);
        assert_eq!(records[2]["line"], 2);
    }

    #[test]
    fn first_record_is_meta_with_tag() {
        let mut em = Emitter::capturing(0);
        em.emit(&json!({"type": "match"})).unwrap();
        let records = em.into_records();
        assert_eq!(records[0]["type"], "meta");
        assert_eq!(records[0]["tag"], super::SMC_TAG);
        assert_eq!(records[0]["tool"], "smc");
    }

    #[test]
    fn header_survives_truncation() {
        // Even with a budget too small for real records, the header is still written
        // so `search` can exclude the output.
        let mut em = Emitter::capturing(1);
        let _ = em.emit(&json!({"type": "match", "data": "aaaa bbbb cccc dddd"}));
        let bytes = em.into_bytes();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(super::SMC_TAG));
    }

    #[test]
    fn budget_truncates() {
        let mut em = Emitter::capturing(1);
        let big = json!({"type": "x", "data": "aaaa bbbb cccc dddd eeee ffff"});
        let ok = em.emit(&big).unwrap();
        assert!(!ok);
        assert!(em.truncated);
    }

    #[test]
    fn zero_budget_is_unlimited() {
        let mut em = Emitter::capturing(0);
        for i in 0..100 {
            assert!(em.emit(&json!({"n": i})).unwrap());
        }
        assert!(!em.truncated);
    }

    #[test]
    fn warn_emits_warning_record() {
        let mut em = Emitter::capturing(0);
        em.warn(Some("foo.jsonl"), "bad line");
        let records = em.into_records();
        // Leading meta header + the warning.
        assert_eq!(records.len(), 2);
        assert_eq!(records[1]["type"], "warning");
    }
}
