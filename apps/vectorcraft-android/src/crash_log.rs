//! A crash report the user can send, since a tablet has no terminal or logcat at hand.
//!
//! While the app runs, its log goes to logcat and to `app.log` in the app's private folder, and
//! the process's stderr (where Rust reports a stack overflow or an abort) goes to `stderr.log`.
//! At the next start, if the last run ended badly (a panic, anything on stderr, or the log
//! stopping while the app was in front), both are combined into `Download/VectorCraft crash
//! report.txt` and the app says so once. Without All files access the report waits in the
//! private folder and is copied to Download as soon as the access is on ([`deliver_pending`]).
//!
//! A freeze isn't a crash: Android offers to close an app that stops responding, which ends it
//! without a trace. So a watchdog ([`watch`]) notices when the UI thread hasn't finished a frame
//! for a few seconds while the app is in front, logs it, and has that thread write where it is
//! stuck (its backtrace) to stderr, which the next report includes.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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
    let _ = std::fs::write(dir.join(PENDING), &report);
    let saved_to = if deliver_from(dir) {
        "Download › VectorCraft crash report.txt".to_string()
    } else {
        "VectorCraft's private folder for now. Turn on All files access for VectorCraft (Settings › Apps › ⋮ › Special access › All files access) and it is copied to Download › VectorCraft crash report.txt".to_string()
    };
    Some(Previous { saved_to })
}

/// A report not yet copied to Download (in the private folder).
const PENDING: &str = "crash report.txt";
const DELIVERED: &str = "crash report (in Download).txt";

/// Copy a waiting report to Download. → whether one was copied.
fn deliver_from(dir: &Path) -> bool {
    let pending = dir.join(PENDING);
    let Ok(report) = std::fs::read(&pending) else { return false };
    if std::fs::write(SHARED_REPORT, report).is_err() {
        return false;
    }
    let _ = std::fs::rename(&pending, dir.join(DELIVERED));
    true
}

/// Copy a report that waited for All files access to Download, if there is one and the access is
/// now on. → whether it did (the app says so).
pub fn deliver_pending() -> bool {
    std::env::var_os("HOME").is_some_and(|home| deliver_from(Path::new(&home)))
}

/// Milliseconds since `EPOCH` when the UI thread last finished a frame.
static BEAT: AtomicU64 = AtomicU64::new(0);
static FRONT: AtomicBool = AtomicBool::new(false);
/// The UI thread (a `pthread_t`), for the watchdog's signal.
static UI_THREAD: AtomicUsize = AtomicUsize::new(0);
static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
/// A frame taking longer than this, in front, is a freeze.
const STALL: Duration = Duration::from_secs(5);

fn now_ms() -> u64 {
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// egui's context, so the watchdog can ask for frames: an idle egui app draws none, and only a
/// frame asked for and not drawn is a freeze.
static CTX: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

/// The UI thread finished a frame.
pub fn beat_with(ctx: &egui::Context) {
    let _ = CTX.get_or_init(|| ctx.clone());
    beat();
}

fn beat() {
    BEAT.store(now_ms(), Ordering::Relaxed);
}

/// Whether the app is in front (a backgrounded app draws no frames, which is no freeze).
pub fn set_front(front: bool) {
    beat();
    FRONT.store(front, Ordering::Relaxed);
}

/// Where a frozen UI thread is: its backtrace, written to stderr by that thread itself.
extern "C" fn dump_backtrace(_signal: libc::c_int) {
    // Not async-signal-safe (it allocates), which is acceptable here: the thread is stuck
    // anyway, and at worst the dump is lost.
    let trace = std::backtrace::Backtrace::force_capture();
    let mut err = std::io::stderr();
    let _ = writeln!(err, "The UI thread stopped responding here:\n{trace}");
    let _ = err.flush();
}

/// Start the freeze watchdog. Call on the UI thread (`android_main`).
pub fn watch() {
    #[allow(unsafe_code)]
    // SAFETY: pthread_self has no preconditions; installing a handler for SIGUSR2, which neither
    // Android's runtime nor the app uses, with a plain `extern "C" fn(c_int)`.
    let installed = unsafe {
        UI_THREAD.store(libc::pthread_self() as usize, Ordering::Relaxed);
        libc::signal(libc::SIGUSR2, dump_backtrace as extern "C" fn(libc::c_int) as libc::sighandler_t) != libc::SIG_ERR
    };
    beat();
    let spawned = std::thread::Builder::new().name("freeze-watchdog".into()).spawn(move || {
        let mut reported = false;
        loop {
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
            std::thread::sleep(Duration::from_secs(1));
            let since = now_ms().saturating_sub(BEAT.load(Ordering::Relaxed));
            if !FRONT.load(Ordering::Relaxed) || since < STALL.as_millis() as u64 {
                if reported {
                    log::warn!("the UI thread is responding again");
                }
                reported = false;
                continue;
            }
            if reported {
                continue;
            }
            reported = true;
            log::error!("{PANIC} the UI thread hasn't finished a frame for {since} ms (frozen)");
            flush();
            let thread = UI_THREAD.load(Ordering::Relaxed);
            if installed && thread != 0 {
                #[allow(unsafe_code)]
                // SAFETY: the UI thread runs for the whole process (android_main), so its id is
                // valid; SIGUSR2 has the handler above.
                unsafe {
                    libc::pthread_kill(thread as libc::pthread_t, libc::SIGUSR2);
                }
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("no freeze watchdog: {e}");
    }
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
