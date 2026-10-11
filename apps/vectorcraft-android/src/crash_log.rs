//! A crash report the user can send, since a tablet has no terminal or logcat at hand.
//!
//! While the app runs, its log goes to logcat and to `app.log` in the app's private folder, and
//! the process's stderr (where Rust reports a stack overflow or an abort) goes to `stderr.log`.
//! At the next start, if the last run ended badly (a panic, anything on stderr, or the log
//! stopping while the app was in front), both are combined into `Download/VectorCraft crash
//! report.txt` and the app says so once.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Written when the app comes to the front, goes to the background, and exits cleanly.
pub const FOREGROUND: &str = "[session] in foreground";
pub const BACKGROUND: &str = "[session] in background";
pub const EXIT: &str = "[session] exit";
/// The panic hook's prefix (lib.rs).
pub const PANIC: &str = "internal error:";

/// Where the report goes: shared storage's Download folder (needs All files access; otherwise
/// it stays in the private folder only).
const SHARED_REPORT: &str = "/storage/emulated/0/Download/VectorCraft crash report.txt";
/// The log is restarted when it grows past this.
const LOG_CAP: u64 = 2 * 1024 * 1024;
/// How much of the previous log a report keeps (its end).
const REPORT_TAIL: usize = 256 * 1024;

struct Logger {
    android: android_logger::AndroidLogger,
    file: Mutex<Option<(File, u64)>>,
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        log::Log::enabled(&self.android, metadata)
    }

    fn log(&self, record: &log::Record) {
        log::Log::log(&self.android, record);
        if !self.enabled(record.metadata()) {
            return;
        }
        // A poisoned lock only means another thread panicked mid-write: keep logging.
        let mut guard = self.file.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((file, written)) = guard.as_mut() {
            let line = format!("{} {} {}: {}\n", seconds(), record.level(), record.target(), record.args());
            if *written > LOG_CAP {
                // Best effort: a log that can't restart keeps growing, which is harmless.
                let _ = file.set_len(0);
                *written = 0;
            }
            if file.write_all(line.as_bytes()).is_ok() {
                *written = written.saturating_add(line.len() as u64);
            }
        }
    }

    fn flush(&self) {
        let mut guard = self.file.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((file, _)) = guard.as_mut() {
            let _ = file.flush();
        }
    }
}

fn seconds() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| format!("{}.{:03}", d.as_secs(), d.subsec_millis()))
        .unwrap_or_default()
}

/// What the start-up found about the previous run.
pub struct Previous {
    /// Where the report was saved, for the notice.
    pub saved_to: String,
}

/// Start logging (to logcat and, with `dir`, to files there) and look at the previous run.
/// Call first thing in `android_main`, before any other thread starts.
pub fn start(dir: Option<&Path>) -> Option<Previous> {
    let config = android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("VectorCraft");
    let previous = dir.and_then(previous_run);
    let file = dir.and_then(|d| File::create(d.join("app.log")).ok()).map(|f| (f, 0));
    let logger = Box::leak(Box::new(Logger { android: android_logger::AndroidLogger::new(config), file: Mutex::new(file) }));
    // Fails only when a logger is already set (a second start): the first one stays.
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Info);
    if let Some(dir) = dir {
        redirect_stderr(&dir.join("stderr.log"));
    }
    previous
}

/// The process's stderr (fd 2) into `path`: Android sends it nowhere.
fn redirect_stderr(path: &Path) {
    let Ok(file) = File::create(path) else { return };
    use std::os::fd::AsRawFd;
    #[allow(unsafe_code)]
    // SAFETY: dup2 on two valid descriptors (the file stays open for the call; fd 2 always
    // exists); it only replaces fd 2, which nothing in Rust owns.
    let ok = unsafe { libc::dup2(file.as_raw_fd(), 2) } >= 0;
    if ok {
        // fd 2 is now its own copy of the file; this handle can close.
        drop(file);
    }
}

fn read_text(path: &Path) -> String {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

/// The end of `text`, at most `max` bytes, cut at a line start.
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut cut = text.len().saturating_sub(max);
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    let rest = text.get(cut..).unwrap_or_default();
    rest.find('\n').and_then(|i| rest.get(i + 1..)).unwrap_or(rest)
}

/// Whether the previous run ended badly, from its log and stderr.
fn crashed(log: &str, stderr: &str) -> bool {
    if !stderr.trim().is_empty() || log.contains(PANIC) {
        return true;
    }
    // The last session marker: still in front, with no clean exit, means it stopped there.
    let last = log.lines().rev().find(|l| l.contains(FOREGROUND) || l.contains(BACKGROUND) || l.contains(EXIT));
    last.is_some_and(|l| l.contains(FOREGROUND))
}

fn previous_run(dir: &Path) -> Option<Previous> {
    let log = read_text(&dir.join("app.log"));
    let stderr = read_text(&dir.join("stderr.log"));
    if !crashed(&log, &stderr) {
        return None;
    }
    let report = format!(
        "VectorCraft {} crash report\n\n== stderr ==\n{}\n\n== log (end) ==\n{}\n",
        env!("CARGO_PKG_VERSION"),
        if stderr.trim().is_empty() { "(empty)" } else { stderr.trim_end() },
        tail(&log, REPORT_TAIL).trim_end()
    );
    let private: PathBuf = dir.join("crash report.txt");
    let _ = std::fs::write(&private, &report);
    let saved_to = match std::fs::write(SHARED_REPORT, &report) {
        Ok(()) => "Download › VectorCraft crash report.txt".to_string(),
        Err(_) => format!("{} (allow All files access to have it saved to Download)", private.display()),
    };
    Some(Previous { saved_to })
}

/// Make sure the log reaches the file (before something that may end the process).
pub fn flush() {
    log::logger().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_that_stopped_in_front_crashed() {
        assert!(crashed(&format!("a\n{FOREGROUND}\nb\n"), ""));
        assert!(!crashed(&format!("{FOREGROUND}\n{BACKGROUND}\n"), ""));
        assert!(!crashed(&format!("{FOREGROUND}\n{EXIT}\n"), ""));
        assert!(crashed("", "thread 'android_main' has overflowed its stack"));
        assert!(crashed(&format!("{PANIC} x"), ""));
    }

    #[test]
    fn tail_cuts_at_a_line() {
        assert_eq!(tail("one\ntwo\nthree\n", 8), "three\n");
        assert_eq!(tail("ab", 8), "ab");
        assert_eq!(tail("ééé\nx", 3), "x");
    }
}
