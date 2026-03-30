use eyre::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

pub struct SessionLog {
    file: BufWriter<File>,
}

impl SessionLog {
    pub fn new(session_dir: &Path) -> Result<Self> {
        let log_path = session_dir.join("session.log");
        let file = File::create(&log_path).context("Failed to create session log")?;
        Ok(Self {
            file: BufWriter::new(file),
        })
    }

    /// Write to file only
    pub fn log(&mut self, line: &str) -> Result<()> {
        writeln!(self.file, "{}", line).context("Failed to write to session log")?;
        Ok(())
    }

    /// Write to both terminal and file
    pub fn println(&mut self, line: &str) -> Result<()> {
        println!("{}", line);
        let stripped = strip_ansi(line);
        writeln!(self.file, "{}", stripped).context("Failed to write to session log")?;
        Ok(())
    }
}

/// Strip ANSI escape sequences from a string
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_creates_file() {
        let dir = tempdir().unwrap();
        let _log = SessionLog::new(dir.path()).unwrap();
        assert!(dir.path().join("session.log").exists());
    }

    #[test]
    fn test_log_writes_to_file() {
        let dir = tempdir().unwrap();
        let mut log = SessionLog::new(dir.path()).unwrap();
        log.log("test line").unwrap();
        drop(log);

        let content = std::fs::read_to_string(dir.path().join("session.log")).unwrap();
        assert!(content.contains("test line"));
    }

    #[test]
    fn test_println_writes_to_file() {
        let dir = tempdir().unwrap();
        let mut log = SessionLog::new(dir.path()).unwrap();
        log.println("hello world").unwrap();
        drop(log);

        let content = std::fs::read_to_string(dir.path().join("session.log")).unwrap();
        assert!(content.contains("hello world"));
    }

    #[test]
    fn test_strip_ansi_removes_codes() {
        assert_eq!(strip_ansi("\x1b[32mgreen\x1b[0m"), "green");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
        assert_eq!(strip_ansi("\x1b[1;34mbold blue\x1b[0m"), "bold blue");
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_println_strips_ansi_in_file() {
        let dir = tempdir().unwrap();
        let mut log = SessionLog::new(dir.path()).unwrap();
        log.println("\x1b[32mgreen text\x1b[0m").unwrap();
        drop(log);

        let content = std::fs::read_to_string(dir.path().join("session.log")).unwrap();
        assert!(content.contains("green text"));
        assert!(!content.contains("\x1b"));
    }

    #[test]
    fn test_multiple_log_entries() {
        let dir = tempdir().unwrap();
        let mut log = SessionLog::new(dir.path()).unwrap();
        log.log("line 1").unwrap();
        log.log("line 2").unwrap();
        log.log("line 3").unwrap();
        drop(log);

        let content = std::fs::read_to_string(dir.path().join("session.log")).unwrap();
        assert!(content.contains("line 1"));
        assert!(content.contains("line 2"));
        assert!(content.contains("line 3"));
    }
}
