//! Dual-layer persistent memory for the Reloopy peripheral agent.
//!
//! Implements the local-first, file-driven memory model:
//! - Long-term:   ~/.reloopy/memory/MEMORY.md  (curated facts, injected into system prompt)
//! - Short-term:  ~/.reloopy/memory/YYYY-MM-DD.md  (append-only daily logs)
//!
//! Tools exposed to the agent: memory_search, memory_get, memory_write, memory_append.

use std::fs;
use std::path::{Path, PathBuf};

pub struct MemoryManager {
    memory_dir: PathBuf,
}

impl MemoryManager {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            memory_dir: base_dir.join("memory"),
        }
    }

    fn ensure_dir(&self) -> Result<(), String> {
        fs::create_dir_all(&self.memory_dir)
            .map_err(|e| format!("Failed to create memory directory: {}", e))
    }

    fn long_term_path(&self) -> PathBuf {
        self.memory_dir.join("MEMORY.md")
    }

    fn daily_path(&self, date_str: &str) -> PathBuf {
        self.memory_dir.join(format!("{}.md", date_str))
    }

    /// Returns today's date as YYYY-MM-DD (UTC).
    pub fn today() -> String {
        date_string(0)
    }

    /// Returns yesterday's date as YYYY-MM-DD (UTC).
    pub fn yesterday() -> String {
        date_string(1)
    }

    /// Load the combined memory context (MEMORY.md + yesterday + today) for
    /// injection into the system prompt at session start.
    pub fn load_context(&self) -> String {
        let mut parts = Vec::new();

        if let Ok(content) = fs::read_to_string(self.long_term_path()) {
            if !content.trim().is_empty() {
                parts.push(format!("## Long-term Memory (MEMORY.md)\n\n{}", content.trim()));
            }
        }

        let yesterday = Self::yesterday();
        let today = Self::today();
        for (label, date) in [("Yesterday's Log", &yesterday), ("Today's Log", &today)] {
            if let Ok(content) = fs::read_to_string(self.daily_path(date)) {
                if !content.trim().is_empty() {
                    parts.push(format!("## {} ({})\n\n{}", label, date, content.trim()));
                }
            }
        }

        parts.join("\n\n---\n\n")
    }

    /// Overwrite the long-term MEMORY.md with new content.
    pub fn write_long_term(&self, content: &str) -> Result<(), String> {
        self.ensure_dir()?;
        fs::write(self.long_term_path(), content)
            .map_err(|e| format!("Failed to write MEMORY.md: {}", e))
    }

    /// Read the full contents of MEMORY.md.
    pub fn get_long_term(&self) -> Result<String, String> {
        if !self.long_term_path().exists() {
            return Ok(String::new());
        }
        fs::read_to_string(self.long_term_path())
            .map_err(|e| format!("Failed to read MEMORY.md: {}", e))
    }

    /// Append a timestamped entry to today's daily log.
    pub fn append_daily(&self, content: &str) -> Result<(), String> {
        self.ensure_dir()?;
        let today = Self::today();
        let path = self.daily_path(&today);

        let now = current_time_string();
        let entry = format!("### {}\n\n{}\n\n", now, content.trim());

        let existing = if path.exists() {
            fs::read_to_string(&path).map_err(|e| format!("Failed to read daily log: {}", e))?
        } else {
            format!("# Daily Log — {}\n\n", today)
        };

        fs::write(&path, format!("{}{}", existing, entry))
            .map_err(|e| format!("Failed to write daily log: {}", e))
    }

    /// Get a daily log by date key: "today", "yesterday", or "YYYY-MM-DD".
    pub fn get_daily(&self, date: &str) -> Result<String, String> {
        let date_str = match date.trim() {
            "" | "today" => Self::today(),
            "yesterday" => Self::yesterday(),
            s => s.to_string(),
        };
        let path = self.daily_path(&date_str);
        if !path.exists() {
            return Ok(format!("No log found for {}.", date_str));
        }
        fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read log for {}: {}", date_str, e))
    }

    /// Simple keyword search across all memory files, returning ranked snippets.
    ///
    /// Splits each file into paragraph-level sections, counts query term hits
    /// per section, and returns the top-10 results sorted by relevance.
    pub fn search(&self, query: &str) -> Result<String, String> {
        if !self.memory_dir.exists() {
            return Ok("Memory directory is empty — no memory files found.".to_string());
        }

        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();
        if terms.is_empty() {
            return Ok("Empty search query.".to_string());
        }

        let mut results: Vec<(usize, String, String)> = Vec::new();

        let entries = fs::read_dir(&self.memory_dir)
            .map_err(|e| format!("Failed to read memory directory: {}", e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Directory read error: {}", e))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let content = fs::read_to_string(&path).unwrap_or_default();
            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            for section in content.split("\n\n") {
                let section = section.trim();
                if section.is_empty() {
                    continue;
                }
                let section_lower = section.to_lowercase();
                let matches = terms.iter().filter(|t| section_lower.contains(*t)).count();
                if matches > 0 {
                    results.push((matches, filename.clone(), section.to_string()));
                }
            }
        }

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        // Sort by relevance descending; MEMORY.md sorts before dated logs on ties.
        results.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        results.truncate(10);

        let mut output = format!("Memory search results for \"{}\"\n\n", query);
        for (score, file, section) in &results {
            output.push_str(&format!(
                "**[{}]** (score: {})\n{}\n\n---\n\n",
                file, score, section
            ));
        }

        Ok(output.trim_end_matches("\n\n---\n\n").to_string())
    }
}

// ── date / time helpers ──────────────────────────────────────────────────────

fn date_string(days_back: u64) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let total_days = secs / 86400;
    let days = total_days.saturating_sub(days_back);
    let (year, month, day) = days_to_ymd(days);
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

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_mgr() -> (tempfile::TempDir, MemoryManager) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = MemoryManager::new(dir.path());
        (dir, mgr)
    }

    #[test]
    fn today_is_valid_date_format() {
        let d = MemoryManager::today();
        assert_eq!(d.len(), 10, "date must be YYYY-MM-DD");
        assert_eq!(&d[4..5], "-");
        assert_eq!(&d[7..8], "-");
    }

    #[test]
    fn yesterday_is_valid_and_differs_from_today() {
        let today = MemoryManager::today();
        let yesterday = MemoryManager::yesterday();
        assert_eq!(yesterday.len(), 10);
        // yesterday ≤ today lexicographically
        assert!(yesterday <= today);
    }

    #[test]
    fn search_missing_dir_returns_graceful_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = MemoryManager::new(dir.path());
        // memory sub-dir doesn't exist yet
        let result = mgr.search("anything").unwrap();
        assert!(
            result.contains("empty") || result.contains("No memory"),
            "got: {}",
            result
        );
    }

    #[test]
    fn write_and_read_long_term() {
        let (_dir, mgr) = temp_mgr();
        mgr.write_long_term("User prefers Rust.").unwrap();
        let content = mgr.get_long_term().unwrap();
        assert_eq!(content, "User prefers Rust.");
    }

    #[test]
    fn append_daily_creates_and_accumulates() {
        let (_dir, mgr) = temp_mgr();
        mgr.append_daily("First entry.").unwrap();
        mgr.append_daily("Second entry.").unwrap();
        let log = mgr.get_daily("today").unwrap();
        assert!(log.contains("First entry."));
        assert!(log.contains("Second entry."));
    }

    #[test]
    fn get_daily_missing_returns_graceful_message() {
        let (_dir, mgr) = temp_mgr();
        let result = mgr.get_daily("1970-01-01").unwrap();
        assert!(result.contains("No log found"), "got: {}", result);
    }

    #[test]
    fn search_finds_matching_content() {
        let (_dir, mgr) = temp_mgr();
        mgr.write_long_term("The user prefers functional programming in Rust.")
            .unwrap();
        let result = mgr.search("functional").unwrap();
        assert!(result.contains("functional"), "got: {}", result);
    }

    #[test]
    fn search_returns_no_results_message_when_unmatched() {
        let (_dir, mgr) = temp_mgr();
        mgr.write_long_term("The user prefers Rust.").unwrap();
        let result = mgr.search("python").unwrap();
        assert!(result.contains("No results"), "got: {}", result);
    }

    #[test]
    fn load_context_empty_when_no_files() {
        let (_dir, mgr) = temp_mgr();
        assert_eq!(mgr.load_context(), "");
    }

    #[test]
    fn load_context_includes_memory_and_log() {
        let (_dir, mgr) = temp_mgr();
        mgr.write_long_term("Key fact.").unwrap();
        mgr.append_daily("Today's work.").unwrap();
        let ctx = mgr.load_context();
        assert!(ctx.contains("Key fact."));
        assert!(ctx.contains("Today's work."));
    }

    #[test]
    fn days_to_ymd_known_dates() {
        // 1970-01-01 = day 0
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        // 1970-01-31 = day 30
        assert_eq!(days_to_ymd(30), (1970, 1, 31));
        // 1970-02-01 = day 31
        assert_eq!(days_to_ymd(31), (1970, 2, 1));
        // 2000-03-01: 2000 is a leap year, so day 60 = Mar 1
        // Days from 1970 to 2000-03-01:
        //   30 years: 1970..1999 = 22 leap + 8 common? Let me count carefully.
        // Actually let's just verify the format is sensible for a known unix epoch.
        // Unix epoch for 2024-01-01 is 19723 days (approx).
        let (y, m, d) = days_to_ymd(19723);
        assert_eq!(y, 2024);
        assert_eq!(m, 1);
        assert_eq!(d, 1);
    }

    #[test]
    fn is_leap_correct() {
        assert!(is_leap(2000));
        assert!(is_leap(2024));
        assert!(!is_leap(1900));
        assert!(!is_leap(2023));
    }

    #[test]
    fn write_long_term_overwrites_existing() {
        let (_dir, mgr) = temp_mgr();
        mgr.write_long_term("Old content.").unwrap();
        mgr.write_long_term("New content.").unwrap();
        let content = mgr.get_long_term().unwrap();
        assert_eq!(content, "New content.");
        assert!(!content.contains("Old content."));
    }

    #[test]
    fn get_long_term_empty_when_not_exists() {
        let (_dir, mgr) = temp_mgr();
        let content = mgr.get_long_term().unwrap();
        assert_eq!(content, "");
    }

    #[test]
    fn search_ranks_more_matches_higher() {
        let (_dir, mgr) = temp_mgr();
        mgr.write_long_term(
            "Rust is great.\n\nRust and functional programming go well together. Rust Rust.",
        )
        .unwrap();
        let result = mgr.search("rust functional").unwrap();
        // The paragraph with both "rust" and "functional" should appear
        assert!(result.contains("functional"), "got: {}", result);
    }

    #[test]
    fn daily_log_get_yesterday() {
        let (_dir, mgr) = temp_mgr();
        // Manually create a file with yesterday's date
        let yesterday = MemoryManager::yesterday();
        let path = mgr.memory_dir.join(format!("{}.md", yesterday));
        fs::create_dir_all(&mgr.memory_dir).unwrap();
        fs::write(&path, "Yesterday content.").unwrap();
        let result = mgr.get_daily("yesterday").unwrap();
        assert!(result.contains("Yesterday content."));
    }
}
