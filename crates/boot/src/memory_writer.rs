//! Minimal append-only memory writer for Boot.
//!
//! Boot uses this module to write rollback events and other system-level
//! context into the shared memory directory (`~/.reloopy/memory/`).
//! The file format is identical to the daily-log format produced by
//! peripheral's `MemoryManager`, so entries appear seamlessly in the
//! agent's memory context on next startup.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Append a structured entry to today's daily log.
///
/// The entry is timestamped and written in the same Markdown format as
/// peripheral's `MemoryManager::append_daily`.
pub fn append_to_daily_log(base_dir: &Path, content: &str) -> Result<(), String> {
    let memory_dir = base_dir.join("memory");
    fs::create_dir_all(&memory_dir)
        .map_err(|e| format!("Failed to create memory directory: {}", e))?;

    let today = date_string();
    let path = memory_dir.join(format!("{}.md", today));

    let needs_header = !path.exists();

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("Failed to open daily log: {}", e))?;

    if needs_header {
        writeln!(file, "# Daily Log — {}\n", today)
            .map_err(|e| format!("Failed to write log header: {}", e))?;
    }

    let now = current_time_string();
    write!(file, "### {}\n\n{}\n\n", now, content.trim())
        .map_err(|e| format!("Failed to write daily log: {}", e))
}

/// Build a structured rollback memory entry.
pub fn format_rollback_entry(
    from_version: &str,
    to_version: &str,
    reason: &str,
    errors: Option<&str>,
    user_feedback: Option<&str>,
) -> String {
    let mut entry = format!(
        "**[SYSTEM ROLLBACK]** {} → {}\n\n**Reason:** {}",
        from_version, to_version, reason,
    );

    if let Some(errors) = errors {
        let truncated = truncate_str(errors, 2000);
        entry.push_str(&format!("\n\n**Errors:**\n```\n{}\n```", truncated));
    }

    if let Some(feedback) = user_feedback {
        entry.push_str(&format!("\n\n**User feedback:** {}", feedback));
    }

    entry
}

fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Find a valid UTF-8 boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn date_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let total_days = secs / 86400;
    let (year, month, day) = days_to_ymd(total_days);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

fn current_time_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    format!("{:02}:{:02} UTC", hours, minutes)
}

fn days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    let mut year = 1970u32;
    loop {
        let days_in_year: u64 = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let months: [u64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u32;
    for &days_in_month in &months {
        if days < days_in_month {
            break;
        }
        days -= days_in_month;
        month += 1;
    }
    (year, month, days as u32 + 1)
}

fn is_leap(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_rollback_entry_basic() {
        let entry = format_rollback_entry("V3", "V2", "hot_swap_timeout", None, None);
        assert!(entry.contains("[SYSTEM ROLLBACK]"));
        assert!(entry.contains("V3 → V2"));
        assert!(entry.contains("hot_swap_timeout"));
    }

    #[test]
    fn format_rollback_entry_with_errors_and_feedback() {
        let entry = format_rollback_entry(
            "V3",
            "V2",
            "compilation_failed",
            Some("error[E0308]: mismatched types"),
            Some("The new version broke the REPL"),
        );
        assert!(entry.contains("mismatched types"));
        assert!(entry.contains("broke the REPL"));
    }

    #[test]
    fn append_to_daily_log_creates_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        append_to_daily_log(dir.path(), "Test entry").unwrap();
        let today = date_string();
        let path = dir.path().join("memory").join(format!("{}.md", today));
        assert!(path.exists());
        let content = fs::read_to_string(path).unwrap();
        assert!(content.contains("Test entry"));
        assert!(content.contains("# Daily Log"));
    }

    #[test]
    fn truncate_str_respects_utf8() {
        let s = "hello 世界 foo";
        let t = truncate_str(s, 8);
        assert!(t.len() <= 8);
        assert!(t.is_char_boundary(t.len()));
    }
}
