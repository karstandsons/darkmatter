//! Backups: a consistent copy of the database plus the stored files, as one
//! `.tar.gz`, safe to take while the server runs. And restoring one.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::db::Db;

/// What's backed up besides the database: everything axiond stores.
const FOLDERS: &[&str] = &["files", "emoji", "pictures", "acme"];

/// Writes `axiond-<time>.tar.gz` into `out_dir` and returns its path.
pub async fn create(db: &Db, data_dir: &Path, out_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("making {}", out_dir.display()))?;
    private_dir(out_dir);
    let stamp = stamp();
    let snapshot = out_dir.join(format!(".snapshot-{stamp}.db"));
    snapshot_db(db, &snapshot).await?;
    let out = out_dir.join(format!("axiond-{stamp}.tar.gz"));
    let partial = out.with_extension("gz.part");
    let (data_dir, snap, part) = (data_dir.to_path_buf(), snapshot.clone(), partial.clone());
    let written = tokio::task::spawn_blocking(move || write_archive(&data_dir, &snap, &part)).await?;
    let _ = std::fs::remove_file(&snapshot);
    written?;
    std::fs::rename(&partial, &out)?;
    Ok(out)
}

/// A consistent copy of the live database (SQLite's `VACUUM INTO`).
pub async fn snapshot_db(db: &Db, to: &Path) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(to);
    sqlx::query("VACUUM INTO ?").bind(to.to_string_lossy().as_ref()).execute(db).await.context("copying the database")?;
    private_file(to);
    Ok(())
}

fn write_archive(data_dir: &Path, snapshot: &Path, out: &Path) -> anyhow::Result<()> {
    let file = create_private(out)?;
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    tar.follow_symlinks(false);
    tar.append_path_with_name(snapshot, "axiond.db")?;
    for folder in FOLDERS {
        let dir = data_dir.join(folder);
        if dir.is_dir() {
            tar.append_dir_all(folder, &dir)?;
        }
    }
    tar.into_inner()?.finish()?.sync_all()?;
    Ok(())
}

/// Unpacks a backup into an empty (or missing) data folder. Only the
/// database and the known folders are taken from it, and nothing outside
/// the data folder is ever written.
pub fn restore(archive: &Path, data_dir: &Path) -> anyhow::Result<()> {
    if data_dir.join("axiond.db").exists() {
        bail!(
            "{} already has a database. Stop axiond and move the old data folder aside first, \
             or restore into a new folder.",
            data_dir.display()
        );
    }
    std::fs::create_dir_all(data_dir)?;
    private_dir(data_dir);
    let file = std::fs::File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut found_db = false;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let top = path.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
        let wanted = path == Path::new("axiond.db") || FOLDERS.contains(&top.as_str());
        if !wanted || !matches!(entry.header().entry_type(), tar::EntryType::Regular | tar::EntryType::Directory) {
            continue;
        }
        // `unpack_in` refuses paths that would escape the data folder.
        if !entry.unpack_in(data_dir)? {
            bail!("the backup has a path outside the data folder: {}", path.display());
        }
        found_db |= path == Path::new("axiond.db");
    }
    if !found_db {
        bail!("{} isn't an axiond backup (no axiond.db in it)", archive.display());
    }
    private_file(&data_dir.join("axiond.db"));
    Ok(())
}

/// Removes all but the newest `keep` backups in `dir`.
pub fn prune(dir: &Path, keep: usize) -> anyhow::Result<()> {
    prune_named(dir, "axiond-", ".tar.gz", keep)
}

/// How many copies of the database from before an upgrade are kept.
pub const UPGRADE_COPIES: usize = 3;

/// Removes all but the newest [`UPGRADE_COPIES`] `before-upgrade-*.db` in `dir`.
pub fn prune_upgrade_copies(dir: &Path) -> anyhow::Result<()> {
    prune_named(dir, "before-upgrade-", ".db", UPGRADE_COPIES)
}

/// Keeps the newest `keep` files named `prefix…suffix` whose names sort by time.
fn prune_named(dir: &Path, prefix: &str, suffix: &str, keep: usize) -> anyhow::Result<()> {
    let mut backups: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix) && n.to_string_lossy().ends_with(suffix)))
        .collect();
    // The names sort by time.
    backups.sort();
    let extra = backups.len().saturating_sub(keep);
    for old in &backups[..extra] {
        std::fs::remove_file(old)?;
    }
    Ok(())
}

/// `20261019-153000`, in UTC, sortable.
fn stamp() -> String {
    let secs = crate::ids::now() / 1000;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rest / 3600, rest % 3600 / 60, rest % 60)
}

fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

fn private_file(path: &Path) {
    #[cfg(unix)]
    let _ = std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    #[cfg(not(unix))]
    let _ = path;
}

fn private_dir(path: &Path) {
    #[cfg(unix)]
    let _ = std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_sort_by_time() {
        let s = stamp();
        assert_eq!(s.len(), 15);
        assert!(s.starts_with("20"));
        assert_eq!(&s[8..9], "-");
    }

    #[test]
    fn pruning_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["axiond-20260101-000000.tar.gz", "axiond-20260102-000000.tar.gz", "axiond-20260103-000000.tar.gz", "other.txt"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        prune(dir.path(), 2).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["axiond-20260102-000000.tar.gz", "axiond-20260103-000000.tar.gz", "other.txt"]);

        let dir = tempfile::tempdir().unwrap();
        for at in [1_700_000_000_000_i64, 1_700_000_000_001, 1_760_000_000_000, 1_800_000_000_000, 1_800_000_000_002] {
            std::fs::write(dir.path().join(format!("before-upgrade-{at}.db")), b"x").unwrap();
        }
        std::fs::write(dir.path().join("axiond-20260101-000000.tar.gz"), b"x").unwrap();
        prune_upgrade_copies(dir.path()).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["axiond-20260101-000000.tar.gz", "before-upgrade-1760000000000.db", "before-upgrade-1800000000000.db", "before-upgrade-1800000000002.db"]);
    }
}
