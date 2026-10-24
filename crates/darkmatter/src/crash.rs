//! A crash log. A panic writes what happened (where, the message, a
//! backtrace) to `crash.txt` in Darkmatter's state folder, and the next
//! start says "Darkmatter crashed" with where that file is, so it can go
//! with a bug report. The file stays on this computer, readable only by you:
//! it can hold some of the text that made it crash.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The newest crash files kept.
const KEEP: usize = 10;

fn dir() -> Option<PathBuf> {
    Some(dirs::state_dir().or_else(dirs::data_local_dir)?.join("darkmatter/crashes"))
}

/// Writes every panic to the crash file, then panics as before (the message
/// on stderr; a panic in the window's thread still ends Darkmatter).
pub fn install() {
    let Some(dir) = dir() else { return };
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Err(e) = write(&dir, &report(info)) {
            eprintln!("couldn't write the crash log in {}: {e}", dir.display());
        }
        default(info);
    }));
}

fn report(info: &std::panic::PanicHookInfo) -> String {
    let thread = std::thread::current();
    let message = info.payload().downcast_ref::<&str>().copied().or_else(|| info.payload().downcast_ref::<String>().map(String::as_str)).unwrap_or("(no message)");
    let at = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
    format!(
        "Darkmatter {} crashed at {}\nthread: {}\nwhere: {at}\nwhat: {message}\n\n{}\n",
        env!("CARGO_PKG_VERSION"),
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        thread.name().unwrap_or("(unnamed)"),
        std::backtrace::Backtrace::force_capture(),
    )
}

/// Adds to `crash.txt` (several threads can panic before Darkmatter ends).
fn write(dir: &Path, report: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(dir.join("crash.txt"))?.write_all(report.as_bytes())
}

/// The crash log of the last run, if it crashed: renamed with its time so
/// the next crash doesn't add to it, and the oldest beyond [`KEEP`] removed.
pub fn take_last() -> Option<PathBuf> {
    take_last_in(&dir()?)
}

fn take_last_in(dir: &Path) -> Option<PathBuf> {
    let new = dir.join("crash.txt");
    let when = std::fs::metadata(&new).ok()?.modified().map(chrono::DateTime::<chrono::Local>::from).unwrap_or_else(|_| chrono::Local::now());
    let mut kept = dir.join(format!("crash-{}.txt", when.format("%Y-%m-%d-%H%M%S")));
    for n in 2.. {
        if !kept.exists() {
            break;
        }
        kept = dir.join(format!("crash-{}-{n}.txt", when.format("%Y-%m-%d-%H%M%S")));
    }
    if let Err(e) = std::fs::rename(&new, &kept) {
        tracing::warn!("couldn't keep the crash log: {e}");
        return Some(new);
    }
    let mut old: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("crash-"))).collect();
    // Named by time: sorted, the oldest first.
    old.sort();
    for p in old.iter().take(old.len().saturating_sub(KEEP)) {
        let _ = std::fs::remove_file(p);
    }
    Some(kept)
}

/// What the next start says.
pub fn notice(path: &Path) -> String {
    format!("Darkmatter crashed. What happened is in {} — please send it with a bug report.", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_is_written_and_shown_once() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(take_last_in(dir.path()), None, "no crash");
        let crashes = dir.path().join("crashes");
        write(&crashes, "first\n").unwrap();
        write(&crashes, "second\n").unwrap();
        let kept = take_last_in(&crashes).unwrap();
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "first\nsecond\n");
        assert!(kept.file_name().unwrap().to_string_lossy().starts_with("crash-2"));
        assert_eq!(take_last_in(&crashes), None, "shown once");
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&kept).unwrap().permissions()) & 0o777, 0o600);
        // Only the newest are kept.
        for i in 0..KEEP + 3 {
            std::fs::write(crashes.join(format!("crash-2000-01-01-0000{i:02}.txt")), "").unwrap();
        }
        write(&crashes, "third\n").unwrap();
        let newest = take_last_in(&crashes).unwrap();
        let left: Vec<_> = std::fs::read_dir(&crashes).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(left.len(), KEEP);
        assert!(left.contains(&newest) && left.contains(&kept));
    }

    #[test]
    fn a_report_says_where_and_what() {
        let caught = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let into = caught.clone();
        let previous = std::panic::take_hook();
        // Only this test's thread: others run meanwhile.
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() == Some("crashing") {
                *into.lock().unwrap() = report(info);
            }
        }));
        let _ = std::thread::Builder::new().name("crashing".into()).spawn(|| panic!("byte index 3 is not a char boundary")).unwrap().join();
        std::panic::set_hook(previous);
        let r = caught.lock().unwrap().clone();
        assert!(r.starts_with(&format!("Darkmatter {} crashed at ", env!("CARGO_PKG_VERSION"))), "{r}");
        assert!(r.contains("thread: crashing") && r.contains("what: byte index 3 is not a char boundary") && r.contains("src/crash.rs:"), "{r}");
    }
}
