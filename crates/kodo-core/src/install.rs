//! The only code that writes inside a WoW folder: archive checks, extraction, an atomic
//! folder swap with backups, the stash and disabled folders, and restoring saved settings.
//!
//! Staging dirs are created under `Interface/` (the AddOns folder's parent) so every swap step
//! is a same-volume rename. Backups live in kodo's cache, which may be on another volume, so
//! moves there fall back to copy + delete after the swap has already committed.

use std::fs::File;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::addon::AddonRef;
use crate::lockfile::LockEntry;
use crate::{Error, Result, io_err};

/// Refuse archives that expand past this, or hold more entries than this.
const MAX_UNPACKED_BYTES: u64 = 1 << 30;
const MAX_ENTRIES: usize = 50_000;
/// File in each backup dir recording the lockfile entry it came from.
pub const BACKUP_ENTRY: &str = "kodo-entry.toml";

/// Addon folders extracted to a staging dir next to AddOns, ready to swap in.
#[derive(Debug)]
pub struct Staged {
    dir: TempDir,
    pub folders: Vec<String>,
}

impl Staged {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Check and extract a downloaded zip (spec steps 2 to 4).
pub fn extract(zip_path: &Path, expected_size: Option<u64>, addons: &Path) -> Result<Staged> {
    let bad = |message: String| Error::BadArchive {
        path: zip_path.to_path_buf(),
        message,
    };
    let file = File::open(zip_path).map_err(io_err("open", zip_path))?;
    let size = file.metadata().map_err(io_err("read", zip_path))?.len();
    if let Some(expected) = expected_size.filter(|&e| e != size) {
        return Err(bad(format!("expected {expected} bytes, got {size}")));
    }
    let mut archive = zip::ZipArchive::new(file).map_err(|e| bad(e.to_string()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(bad(format!("{} entries is too many", archive.len())));
    }

    // Validate every entry before writing anything.
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i).map_err(|e| bad(e.to_string()))?;
        if entry.enclosed_name().is_none() {
            return Err(bad(format!("unsafe path {:?}", entry.name())));
        }
        total = total.saturating_add(entry.size());
    }
    if total > MAX_UNPACKED_BYTES {
        return Err(bad(format!("expands to {total} bytes")));
    }

    let parent = staging_parent(addons)?;
    let dir = tempfile::Builder::new()
        .prefix(".kodo-new-")
        .tempdir_in(&parent)
        .map_err(io_err("create a staging dir in", &parent))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| bad(e.to_string()))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(bad(format!("unsafe path {:?}", entry.name())));
        };
        let out = dir.path().join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(io_err("create", &out))?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p).map_err(io_err("create", p))?;
        }
        let mut f = File::create(&out).map_err(io_err("create", &out))?;
        std::io::copy(&mut entry, &mut f).map_err(io_err("extract", &out))?;
    }

    let folders = toc_folders(dir.path())?;
    if folders.is_empty() {
        return Err(bad("no top-level folder contains a .toc file".into()));
    }
    Ok(Staged { dir, folders })
}

/// Top-level folders holding at least one `.toc`, sorted.
fn toc_folders(root: &Path) -> Result<Vec<String>> {
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(root).map_err(io_err("read", root))? {
        let entry = entry.map_err(io_err("read", root))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let has_toc = std::fs::read_dir(&path)
            .map_err(io_err("read", &path))?
            .flatten()
            .any(|f| {
                f.file_name()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with(".toc")
            });
        if has_toc {
            folders.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    folders.sort();
    Ok(folders)
}

fn staging_parent(addons: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(addons).map_err(io_err("create", addons))?;
    addons
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::NotAClient(addons.to_path_buf()))
}

/// Result of a committed swap.
#[derive(Debug, Default)]
pub struct Swapped {
    /// Set when the replaced folders could not be moved into the backup dir (the swap itself
    /// still succeeded; only rollback for this version is lost).
    pub backup_warning: Option<String>,
}

/// Replace `old` folders in `addons` with `new` folders from `staged_root` (spec step 6).
///
/// Either every new folder ends up in place and every old folder is in `backup_dir`, or the
/// AddOns dir is left exactly as it was. Pass an empty `new` to remove an addon into its backup.
pub fn swap(
    addons: &Path,
    staged_root: &Path,
    new: &[String],
    old: &[String],
    backup_dir: &Path,
) -> Result<Swapped> {
    swap_inner(addons, staged_root, new, old, backup_dir, None)
}

/// `fail_at` injects an error before the n-th rename, so tests can prove the restore path.
fn swap_inner(
    addons: &Path,
    staged_root: &Path,
    new: &[String],
    old: &[String],
    backup_dir: &Path,
    fail_at: Option<usize>,
) -> Result<Swapped> {
    let parent = staging_parent(addons)?;
    let hold = tempfile::Builder::new()
        .prefix(".kodo-old-")
        .tempdir_in(&parent)
        .map_err(io_err("create a staging dir in", &parent))?;

    let mut step = 0usize;
    let mut rename = |from: &Path, to: &Path| -> Result<()> {
        step += 1;
        if fail_at == Some(step) {
            return Err(io_err("rename", from)(std::io::Error::other(
                "injected failure",
            )));
        }
        std::fs::rename(from, to).map_err(io_err("rename", from))
    };

    // Move the current folders aside.
    let mut held: Vec<&String> = Vec::new();
    let mut placed: Vec<&String> = Vec::new();
    let result = (|| {
        for f in old {
            let src = addons.join(f);
            if src.exists() {
                rename(&src, &hold.path().join(f))?;
                held.push(f);
            }
        }
        for f in new {
            let dst = addons.join(f);
            if dst.exists() {
                // A folder outside `old` is in the way; callers check ownership, so this is a race.
                return Err(Error::Unmanaged(f.clone()));
            }
            rename(&staged_root.join(f), &dst)?;
            placed.push(f);
        }
        Ok(())
    })();

    if let Err(e) = result {
        // Undo in reverse; best effort, the original error is what matters.
        for f in placed.iter().rev() {
            let _ = std::fs::rename(addons.join(f), staged_root.join(f));
        }
        for f in held.iter().rev() {
            let _ = std::fs::rename(hold.path().join(f), addons.join(f));
        }
        return Err(e);
    }

    // Committed. Park the old folders in the backup dir.
    let mut out = Swapped::default();
    if !held.is_empty()
        && let Err(e) = park(hold.path(), &held, backup_dir)
    {
        out.backup_warning = Some(e.to_string());
    }
    Ok(out)
}

fn park(from: &Path, folders: &[&String], backup_dir: &Path) -> Result<()> {
    if backup_dir.exists() {
        std::fs::remove_dir_all(backup_dir).map_err(io_err("clear", backup_dir))?;
    }
    std::fs::create_dir_all(backup_dir).map_err(io_err("create", backup_dir))?;
    for f in folders {
        move_dir(&from.join(f), &backup_dir.join(f))?;
    }
    Ok(())
}

/// Copy a backup's folders into a fresh staging dir next to AddOns, ready for [`swap`].
pub fn stage_backup(backup_dir: &Path, folders: &[String], addons: &Path) -> Result<Staged> {
    let parent = staging_parent(addons)?;
    let dir = tempfile::Builder::new()
        .prefix(".kodo-new-")
        .tempdir_in(&parent)
        .map_err(io_err("create a staging dir in", &parent))?;
    for f in folders {
        copy_dir(&backup_dir.join(f), &dir.path().join(f))?;
    }
    Ok(Staged {
        dir,
        folders: folders.to_vec(),
    })
}

/// Where a client's addons from inactive profiles wait: `Interface/.kodo-stash`, beside
/// AddOns, so stashing and restoring are same-volume renames and WoW never loads them.
pub fn stash_root(addons: &Path) -> Result<PathBuf> {
    Ok(staging_parent(addons)?.join(".kodo-stash"))
}

/// Addons waiting in the stash, without creating anything.
pub fn stashed(addons: &Path) -> Vec<AddonRef> {
    addons
        .parent()
        .map(|p| all_backups(&p.join(".kodo-stash")))
        .unwrap_or_default()
        .into_iter()
        .map(|(_, e)| e.id)
        .collect()
}

/// Move an addon's folders out of AddOns into the stash, keeping one stashed copy per addon.
/// All or nothing: on failure, folders already moved are put back.
pub fn stash(addons: &Path, entry: &LockEntry, root: &Path) -> Result<()> {
    prune_backups(root, entry, 0)?;
    let dir = backup_dir(root, entry);
    std::fs::create_dir_all(&dir).map_err(io_err("create", &dir))?;
    let present: Vec<&String> = entry
        .folders
        .iter()
        .filter(|f| addons.join(f).exists())
        .collect();
    move_all(&present, addons, &dir)?;
    write_backup_entry(&dir, entry, None)
}

/// Move a stashed addon back into AddOns; `None` when nothing is stashed for `id`.
pub fn unstash(addons: &Path, id: &AddonRef, root: &Path) -> Result<Option<LockEntry>> {
    let Some((dir, entry)) = all_backups(root).into_iter().find(|(_, e)| &e.id == id) else {
        return Ok(None);
    };
    if let Some(f) = entry.folders.iter().find(|f| addons.join(f).exists()) {
        return Err(Error::Unmanaged(f.clone()));
    }
    let stashed: Vec<&String> = entry
        .folders
        .iter()
        .filter(|f| dir.join(f).exists())
        .collect();
    move_all(&stashed, &dir, addons)?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(Some(entry))
}

/// Where disabled folders wait: `Interface/.kodo-disabled`, out of WoW's sight.
pub fn disabled_root(addons: &Path) -> Option<PathBuf> {
    addons.parent().map(|p| p.join(".kodo-disabled"))
}

/// Folders in the disabled dir, sorted.
pub fn disabled(addons: &Path) -> Vec<String> {
    let Some(Ok(entries)) = disabled_root(addons).map(std::fs::read_dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort_by_key(|f| f.to_lowercase());
    out
}

/// Move folders out of AddOns into the disabled dir (all or nothing).
pub fn disable(addons: &Path, folders: &[String]) -> Result<()> {
    let root = staging_parent(addons)?.join(".kodo-disabled");
    std::fs::create_dir_all(&root).map_err(io_err("create", &root))?;
    if let Some(f) = folders.iter().find(|f| root.join(f).exists()) {
        return Err(Error::Disabled(f.clone()));
    }
    let present: Vec<&String> = folders.iter().filter(|f| addons.join(f).exists()).collect();
    move_all(&present, addons, &root)
}

/// Move disabled folders back into AddOns (all or nothing).
pub fn enable(addons: &Path, folders: &[String]) -> Result<()> {
    let root = staging_parent(addons)?.join(".kodo-disabled");
    if let Some(f) = folders.iter().find(|f| addons.join(f).exists()) {
        return Err(Error::Unmanaged(f.clone()));
    }
    let parked: Vec<&String> = folders.iter().filter(|f| root.join(f).exists()).collect();
    move_all(&parked, &root, addons)
}

/// Write saved-settings files under `wtf`, each atomically. Paths come from
/// [`crate::wtf::read`], which already refused anything escaping the folder.
pub fn restore_files(wtf: &Path, files: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    for (rel, data) in files {
        let dest = wtf.join(rel);
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(io_err("create", dir))?;
        }
        crate::config::write_atomic(&dest, data)?;
    }
    Ok(())
}

/// Move `folders` from one dir to another, undoing the moves made so far if one fails.
fn move_all(folders: &[&String], from: &Path, to: &Path) -> Result<()> {
    for (i, f) in folders.iter().enumerate() {
        if let Err(e) = move_dir(&from.join(f), &to.join(f)) {
            for done in folders[..i].iter().rev() {
                let _ = move_dir(&to.join(done), &from.join(done));
            }
            return Err(e);
        }
    }
    Ok(())
}

/// Rename, falling back to copy + delete across volumes.
fn move_dir(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_dir(from, to)?;
    std::fs::remove_dir_all(from).map_err(io_err("remove", from))
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(io_err("create", to))?;
    for entry in std::fs::read_dir(from).map_err(io_err("read", from))? {
        let entry = entry.map_err(io_err("read", from))?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().map_err(io_err("read", &src))?.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(io_err("copy", &src))?;
        }
    }
    Ok(())
}

/// Backup dir for one version of one addon: `<backups>/<id>/<version>/`.
pub fn backup_dir(backups: &Path, entry: &LockEntry) -> PathBuf {
    backups
        .join(slug(&entry.id.to_string()))
        .join(slug(&entry.version))
}

/// Record which entry a backup dir holds, so rollback and undo-remove can restore it, and
/// which settings snapshot was taken just before it was replaced.
pub fn write_backup_entry(dir: &Path, entry: &LockEntry, wtf: Option<&str>) -> Result<()> {
    let made = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let record = BackupRecord {
        made,
        wtf: wtf.map(str::to_owned),
        entry: entry.clone(),
    };
    let text = toml::to_string_pretty(&record).map_err(|e| Error::BadFile {
        path: dir.join(BACKUP_ENTRY),
        message: e.to_string(),
    })?;
    crate::config::write_atomic(&dir.join(BACKUP_ENTRY), text.as_bytes())
}

/// The settings snapshot recorded with a backup, if any.
pub fn backup_wtf(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(BACKUP_ENTRY)).ok()?;
    toml::from_str::<BackupRecord>(&text).ok()?.wtf
}

/// Backups for one addon, most recently made first.
pub fn list_backups(backups: &Path, entry: &LockEntry) -> Vec<(PathBuf, LockEntry)> {
    newest_first(backups_in(&backups.join(slug(&entry.id.to_string()))))
}

/// Every backed-up addon version under a client's backups dir, most recently made first.
pub fn all_backups(backups: &Path) -> Vec<(PathBuf, LockEntry)> {
    let Ok(addons) = std::fs::read_dir(backups) else {
        return Vec::new();
    };
    newest_first(
        addons
            .flatten()
            .flat_map(|a| backups_in(&a.path()))
            .collect(),
    )
}

/// Contents of a backup's entry file. `made` (unix nanoseconds) orders backups; file mtimes
/// are too coarse on some filesystems to tell back-to-back backups apart.
#[derive(serde::Serialize, serde::Deserialize)]
struct BackupRecord {
    made: u64,
    /// File name of the [`crate::wtf`] snapshot taken right before this backup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wtf: Option<String>,
    entry: LockEntry,
}

type Backup = (PathBuf, LockEntry, u64);

/// Version dirs of one addon, with when each backup was made.
fn backups_in(addon_dir: &Path) -> Vec<Backup> {
    let Ok(versions) = std::fs::read_dir(addon_dir) else {
        return Vec::new();
    };
    versions
        .flatten()
        .filter_map(|v| {
            let path = v.path();
            let text = std::fs::read_to_string(path.join(BACKUP_ENTRY)).ok()?;
            let record: BackupRecord = toml::from_str(&text).ok()?;
            Some((path, record.entry, record.made))
        })
        .collect()
}

fn newest_first(mut backups: Vec<Backup>) -> Vec<(PathBuf, LockEntry)> {
    backups.sort_by_key(|b| std::cmp::Reverse(b.2));
    backups.into_iter().map(|(p, e, _)| (p, e)).collect()
}

/// Keep the newest `keep` backups of an addon (spec step 7).
pub fn prune_backups(backups: &Path, entry: &LockEntry, keep: usize) -> Result<()> {
    for (dir, _) in list_backups(backups, entry).into_iter().skip(keep) {
        std::fs::remove_dir_all(&dir).map_err(io_err("remove", &dir))?;
    }
    Ok(())
}

/// Filesystem-safe name for ids and versions.
fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = File::open(path).map_err(io_err("open", path))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(io_err("read", path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;
    use std::path::Path;

    use crate::Error;
    use crate::install::{extract, swap, swap_inner};

    /// Build a zip at `path` from `(name, contents)` pairs; a trailing `/` makes a dir entry.
    pub(crate) fn make_zip(path: &Path, files: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in files {
            if name.ends_with('/') {
                zip.add_directory(*name, opts).unwrap();
            } else {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
        }
        zip.finish().unwrap();
    }

    fn addons_dir(root: &Path) -> std::path::PathBuf {
        let addons = root.join("_forever_/Interface/AddOns");
        std::fs::create_dir_all(&addons).unwrap();
        addons
    }

    #[test]
    fn rejects_zip_slip_and_tocless_archives() {
        let tmp = tempfile::tempdir().unwrap();
        let addons = addons_dir(tmp.path());

        let evil = tmp.path().join("evil.zip");
        make_zip(
            &evil,
            &[("Good/Good.toc", "## Title: x"), ("../escape.txt", "boom")],
        );
        let err = extract(&evil, None, &addons).unwrap_err();
        assert!(matches!(err, Error::BadArchive { .. }), "{err}");
        assert!(!tmp.path().join("_forever_/escape.txt").exists());

        let notoc = tmp.path().join("notoc.zip");
        make_zip(&notoc, &[("Thing/readme.md", "hi")]);
        assert!(matches!(
            extract(&notoc, None, &addons),
            Err(Error::BadArchive { .. })
        ));

        let ok = tmp.path().join("ok.zip");
        make_zip(
            &ok,
            &[("A/A.toc", ""), ("A_Mod/A_Mod.toc", ""), ("stray.txt", "")],
        );
        assert!(matches!(
            extract(&ok, Some(1), &addons),
            Err(Error::BadArchive { .. })
        ));
        let staged = extract(&ok, None, &addons).unwrap();
        assert_eq!(staged.folders, ["A", "A_Mod"]);
    }

    #[test]
    fn swap_failure_leaves_old_version_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let addons = addons_dir(tmp.path());
        for f in ["A", "A_Old"] {
            std::fs::create_dir_all(addons.join(f)).unwrap();
            std::fs::write(addons.join(f).join("v"), "old").unwrap();
        }
        let zip = tmp.path().join("new.zip");
        make_zip(
            &zip,
            &[("A/A.toc", ""), ("A/v", "new"), ("A_New/A_New.toc", "")],
        );
        let old = ["A".to_owned(), "A_Old".to_owned()];
        let backup = tmp.path().join("backups/a/1");

        // Steps: 1-2 hold the old folders, 3-4 place the new ones. Fail on the last placement.
        let staged = extract(&zip, None, &addons).unwrap();
        let err = swap_inner(
            &addons,
            staged.path(),
            &staged.folders,
            &old,
            &backup,
            Some(4),
        );
        assert!(err.is_err());
        assert_eq!(std::fs::read_to_string(addons.join("A/v")).unwrap(), "old");
        assert!(addons.join("A_Old").is_dir());
        assert!(!addons.join("A_New").exists());
        assert!(!backup.exists());
        drop(staged);

        // The real swap then commits and parks the old version.
        let staged = extract(&zip, None, &addons).unwrap();
        let done = swap(&addons, staged.path(), &staged.folders, &old, &backup).unwrap();
        assert!(done.backup_warning.is_none());
        assert_eq!(std::fs::read_to_string(addons.join("A/v")).unwrap(), "new");
        assert!(addons.join("A_New").is_dir() && !addons.join("A_Old").exists());
        assert_eq!(std::fs::read_to_string(backup.join("A/v")).unwrap(), "old");
        drop(staged);

        // No staging dirs are left behind next to AddOns.
        let leftovers: Vec<_> = std::fs::read_dir(addons.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".kodo-"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
