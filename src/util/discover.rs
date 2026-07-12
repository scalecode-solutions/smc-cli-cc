/// Session file discovery — finds all JSONL conversation logs under ~/.claude/projects.
use std::path::{Path, PathBuf};

use anyhow::Result;

// ── SessionFile ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SessionFile {
    pub path: PathBuf,
    pub session_id: String,
    pub project_name: String,
    pub size_bytes: u64,
}

impl SessionFile {
    pub fn size_human(&self) -> String {
        let b = self.size_bytes;
        if b < 1024 {
            format!("{}B", b)
        } else if b < 1024 * 1024 {
            format!("{:.1}KB", b as f64 / 1024.0)
        } else if b < 1024 * 1024 * 1024 {
            format!("{:.1}MB", b as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.2}GB", b as f64 / (1024.0 * 1024.0 * 1024.0))
        }
    }
}

// ── Discovery ──────────────────────────────────────────────────────────────

/// Resolve the Claude projects directory.
pub fn claude_dir(path_override: Option<&str>) -> Result<PathBuf> {
    let dir = if let Some(p) = path_override {
        PathBuf::from(p)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        Path::new(&home).join(".claude").join("projects")
    };
    anyhow::ensure!(dir.exists(), "Claude projects directory not found at {}", dir.display());
    Ok(dir)
}

/// Discover all JSONL session files, sorted largest-first.
///
/// Files can vanish mid-scan (Claude Code actively writes/rotates these), so
/// per-file metadata failures skip the file rather than aborting the run.
pub fn discover_jsonl_files(base: &Path) -> Result<Vec<SessionFile>> {
    let mut files = Vec::new();

    if !base.is_dir() {
        return Ok(files);
    }

    for entry in std::fs::read_dir(base)? {
        let Ok(entry) = entry else { continue };
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        let Ok(dir_entries) = std::fs::read_dir(&project_dir) else { continue };
        let mut jsonl: Vec<(PathBuf, u64)> = Vec::new();
        for file_entry in dir_entries {
            let Ok(file_entry) = file_entry else { continue };
            let path = file_entry.path();
            if path.extension().is_some_and(|e| e == "jsonl") && path.is_file() {
                let Ok(metadata) = std::fs::metadata(&path) else { continue };
                jsonl.push((path, metadata.len()));
            }
        }
        if jsonl.is_empty() {
            continue;
        }

        // The dir name dash-encodes the project path, which is ambiguous
        // ('-' encodes both '/' and a literal dash). The records inside carry
        // the exact `cwd`, so prefer that; fall back to the dir-name heuristic.
        let dir_name = entry.file_name();
        let project_name = project_name_from_cwd(&jsonl)
            .unwrap_or_else(|| extract_project_name(dir_name.to_str().unwrap_or("")));

        for (path, size_bytes) in jsonl {
            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            files.push(SessionFile {
                path,
                session_id,
                project_name: project_name.clone(),
                size_bytes,
            });
        }
    }

    files.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    Ok(files)
}

/// Read the exact project name from the `cwd` field of the first few records
/// of up to three session files in the project directory.
fn project_name_from_cwd(jsonl: &[(PathBuf, u64)]) -> Option<String> {
    use std::io::BufRead;
    for (path, _) in jsonl.iter().take(3) {
        let Ok(f) = std::fs::File::open(path) else { continue };
        let reader = std::io::BufReader::new(f);
        for line in reader.lines().take(20) {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
                if let Some(name) = Path::new(cwd).file_name().and_then(|n| n.to_str()) {
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Find a session by exact ID or unique prefix.
pub fn find_session<'a>(
    files: &'a [SessionFile],
    query: &str,
) -> Result<&'a SessionFile> {
    if let Some(f) = files.iter().find(|f| f.session_id == query) {
        return Ok(f);
    }
    let matches: Vec<_> = files
        .iter()
        .filter(|f| f.session_id.starts_with(query))
        .collect();
    match matches.len() {
        0 => anyhow::bail!("no session found matching '{}'", query),
        1 => Ok(matches[0]),
        n => anyhow::bail!(
            "ambiguous session ID '{}' ({} matches) — provide more characters",
            query,
            n
        ),
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Heuristic fallback when no `cwd` is available: everything after a
/// "github"-like segment (case-insensitive), joined with dashes. Inherently
/// ambiguous — the dash encoding loses the distinction between '/' and '-'.
fn extract_project_name(dir_name: &str) -> String {
    let parts: Vec<&str> = dir_name.split('-').collect();

    if let Some(pos) = parts.iter().position(|p| p.eq_ignore_ascii_case("github")) {
        let project_parts = &parts[pos + 1..];
        if project_parts.is_empty() {
            dir_name.to_string()
        } else {
            project_parts.join("-")
        }
    } else {
        parts
            .iter()
            .rfind(|p| !p.is_empty() && **p != "Users")
            .unwrap_or(&dir_name)
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_github_project() {
        assert_eq!(extract_project_name("-Users-travis-GitHub-myapp"), "myapp");
    }

    #[test]
    fn github_match_is_case_insensitive() {
        // Real dirs say "Github", not "GitHub" — this used to fall through
        // and return the last dash segment ("cc" for smc-cli-cc).
        assert_eq!(
            extract_project_name("-Users-tmarq-Github-smc-cli-cc"),
            "smc-cli-cc"
        );
    }

    #[test]
    fn extracts_nested_project() {
        assert_eq!(
            extract_project_name("-Users-travis-GitHub-misc-smc_cli"),
            "misc-smc_cli"
        );
    }

    #[test]
    fn fallback_last_segment() {
        assert_eq!(extract_project_name("-Users-travis-something"), "something");
    }

    #[test]
    fn cwd_beats_dir_name_heuristic() {
        let dir = std::env::temp_dir().join(format!("smc-discover-test-{}", std::process::id()));
        let project_dir = dir.join("-Users-x-Github-My-Dashed-Name");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("abc123.jsonl"),
            // Adversarial: garbage line first, cwd only on a later record.
            "not json at all\n{\"type\":\"user\",\"cwd\":\"/Users/x/Github/My-Dashed-Name\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();

        let files = discover_jsonl_files(&dir).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].project_name, "My-Dashed-Name");
        assert_eq!(files[0].session_id, "abc123");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_or_garbage_files_fall_back_to_dir_name() {
        let dir = std::env::temp_dir().join(format!("smc-discover-fb-{}", std::process::id()));
        let project_dir = dir.join("-Users-x-Github-fallback-proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("s1.jsonl"), "\u{0}garbage\n\n").unwrap();

        let files = discover_jsonl_files(&dir).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].project_name, "fallback-proj");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
