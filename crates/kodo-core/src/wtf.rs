//! Snapshots of a client's saved addon settings: every `*.lua` under a `SavedVariables` folder
//! in `WTF` (account-wide and per character), zipped into kodo's data dir before updates,
//! removals and profile switches so a rollback can bring settings back with the code.
//!
//! A snapshot is skipped when nothing changed since the last one (same files, sizes and
//! mtimes), so back-to-back updates cost one walk of the folder, not one zip each.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::{Error, Result, io_err};

/// Snapshots kept per client.
pub const KEEP: usize = 10;
/// Deepest SavedVariables folder: `WTF/Account/<account>/<realm>/<character>/SavedVariables`.
const MAX_DEPTH: usize = 5;
const COMMENT_TAG: &str = "kodo-wtf 1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    /// File name, which is also how restore picks it.
    pub name: String,
    #[serde(skip)]
    pub path: PathBuf,
    /// Unix seconds.
    pub made: u64,
    pub reason: String,
    pub files: usize,
    pub bytes: u64,
}

/// One saved-variables file: its path under `WTF` (with `/`), and on disk.
struct SvFile {
    rel: String,
    path: PathBuf,
    len: u64,
    mtime: u128,
}

/// Snapshot `wtf` into `dir` unless it matches the newest snapshot; `None` when there are no
/// saved settings at all. Keeps the newest [`KEEP`].
pub fn snapshot(wtf: &Path, dir: &Path, reason: &str) -> Result<Option<Snapshot>> {
    let files = saved_variables(wtf);
    if files.is_empty() {
        return Ok(None);
    }
    let fp = fingerprint(&files);
    let newest = list(dir).into_iter().next();
    if let Some(s) = newest.filter(|s| comment_fingerprint(&s.path).as_deref() == Some(&fp)) {
        return Ok(Some(s));
    }

    std::fs::create_dir_all(dir).map_err(io_err("create", dir))?;
    let made = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = format!("wtf-{made}.zip");
    let path = dir.join(&name);
    let tmp = dir.join(format!(".{name}.tmp"));
    let bad = |e: zip::result::ZipError| Error::BadArchive {
        path: tmp.clone(),
        message: e.to_string(),
    };
    let mut zip = zip::ZipWriter::new(File::create(&tmp).map_err(io_err("create", &tmp))?);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut bytes = 0;
    for f in &files {
        let data = std::fs::read(&f.path).map_err(io_err("read", &f.path))?;
        bytes += data.len() as u64;
        zip.start_file(f.rel.as_str(), opts).map_err(bad)?;
        zip.write_all(&data).map_err(io_err("write", &tmp))?;
    }
    zip.set_comment(format!("{COMMENT_TAG}\n{fp}\n{}", one_line(reason)))
        .map_err(bad)?;
    zip.finish().map_err(bad)?;
    std::fs::rename(&tmp, &path).map_err(io_err("rename", &tmp))?;

    for old in list(dir).into_iter().skip(KEEP) {
        std::fs::remove_file(&old.path).map_err(io_err("remove", &old.path))?;
    }
    Ok(Some(Snapshot {
        name,
        path,
        made: u64::try_from(made / 1_000_000_000).unwrap_or(u64::MAX),
        reason: one_line(reason),
        files: files.len(),
        bytes,
    }))
}

/// Snapshots in `dir`, newest first.
pub fn list(dir: &Path) -> Vec<Snapshot> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(u128, Snapshot)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let nanos: u128 = name
                .strip_prefix("wtf-")?
                .strip_suffix(".zip")?
                .parse()
                .ok()?;
            let path = e.path();
            let archive = zip::ZipArchive::new(File::open(&path).ok()?).ok()?;
            let comment = String::from_utf8_lossy(archive.comment()).into_owned();
            let bytes = archive.decompressed_size().unwrap_or(0);
            Some((
                nanos,
                Snapshot {
                    name,
                    made: u64::try_from(nanos / 1_000_000_000).unwrap_or(u64::MAX),
                    reason: comment.lines().nth(2).unwrap_or_default().to_owned(),
                    files: archive.len(),
                    bytes: u64::try_from(bytes).unwrap_or(u64::MAX),
                    path,
                },
            ))
        })
        .collect();
    out.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    out.into_iter().map(|(_, s)| s).collect()
}

pub fn find(dir: &Path, name: &str) -> Option<Snapshot> {
    list(dir).into_iter().find(|s| s.name == name)
}

/// How many of a snapshot's files sit under `under` (a folder under `WTF`, e.g. a character's).
pub fn count_under(snapshot: &Path, under: &Path) -> usize {
    let Ok(archive) = File::open(snapshot).map(zip::ZipArchive::new) else {
        return 0;
    };
    archive.map_or(0, |a| {
        a.file_names()
            .flatten()
            .filter(|n| Path::new(n.as_ref()).starts_with(under))
            .count()
    })
}

/// A snapshot's files as `(path under WTF, contents)`; with `only`, just the settings of those
/// addon folders (`<folder>.lua`); with `under`, just the files in that folder under `WTF`.
pub fn read(
    snapshot: &Path,
    only: Option<&[String]>,
    under: Option<&Path>,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let bad = |message: String| Error::BadArchive {
        path: snapshot.to_path_buf(),
        message,
    };
    let file = File::open(snapshot).map_err(io_err("open", snapshot))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| bad(e.to_string()))?;
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| bad(e.to_string()))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(bad(format!("unsafe path {:?}", entry.name())));
        };
        let wanted = under.is_none_or(|u| rel.starts_with(u))
            && only.is_none_or(|folders| {
                rel.file_stem()
                    .map(|s| s.to_string_lossy())
                    .is_some_and(|stem| folders.iter().any(|f| f.eq_ignore_ascii_case(&stem)))
            });
        if wanted && !entry.is_dir() {
            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .map_err(|e| bad(e.to_string()))?;
            out.push((rel, data));
        }
    }
    Ok(out)
}

/// When each addon's settings were last written, in Unix seconds, keyed by lowercased folder
/// name (the newest file across the account and every character). WoW writes them for every
/// loaded addon on logout, so this is when an addon was last loaded, not last used.
pub fn last_saved(wtf: &Path) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    for f in saved_variables(wtf) {
        let Some(stem) = Path::new(&f.rel).file_stem() else {
            continue;
        };
        let secs = u64::try_from(f.mtime / 1_000_000_000).unwrap_or(u64::MAX);
        let slot = out
            .entry(stem.to_string_lossy().to_lowercase())
            .or_insert(0);
        *slot = (*slot).max(secs);
    }
    out
}

/// Every `*.lua` in a `SavedVariables` folder under `wtf`, sorted by path.
fn saved_variables(wtf: &Path) -> Vec<SvFile> {
    fn walk(dir: &Path, rel: &str, depth: usize, out: &mut Vec<SvFile>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let is_sv = dir
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("SavedVariables"));
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&e.path(), &child, depth + 1, out);
                }
            } else if is_sv && name.to_ascii_lowercase().ends_with(".lua") {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                out.push(SvFile {
                    rel: child,
                    path: e.path(),
                    len: meta.len(),
                    mtime,
                });
            }
        }
    }
    let mut out = Vec::new();
    walk(wtf, "", 0, &mut out);
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

fn fingerprint(files: &[SvFile]) -> String {
    let mut text = String::new();
    for f in files {
        text.push_str(&format!("{}\t{}\t{}\n", f.rel, f.len, f.mtime));
    }
    format!("{:016x}", crate::fnv1a(text.as_bytes()))
}

fn comment_fingerprint(path: &Path) -> Option<String> {
    let archive = zip::ZipArchive::new(File::open(path).ok()?).ok()?;
    let comment = String::from_utf8_lossy(archive.comment()).into_owned();
    let mut lines = comment.lines();
    (lines.next() == Some(COMMENT_TAG)).then(|| lines.next().unwrap_or_default().to_owned())
}

fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::wtf::{count_under, list, read, snapshot};

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn snapshots_saved_variables_once_per_change() {
        let tmp = tempfile::tempdir().unwrap();
        let (wtf, dir) = (tmp.path().join("WTF"), tmp.path().join("snaps"));
        assert!(snapshot(&wtf, &dir, "empty").unwrap().is_none());

        write(
            &wtf,
            "Account/A#1/SavedVariables/BetterBags.lua",
            "bags = 1",
        );
        write(
            &wtf,
            "Account/A#1/Realm/Char/SavedVariables/Details.lua",
            "d = 1",
        );
        write(&wtf, "Account/A#1/SavedVariables/BetterBags.lua.bak", "old");
        write(&wtf, "Config.wtf", "SET x 1");
        let first = snapshot(&wtf, &dir, "before updating BetterBags")
            .unwrap()
            .unwrap();
        assert_eq!(first.files, 2);
        let again = snapshot(&wtf, &dir, "again").unwrap().unwrap();
        assert_eq!(
            again.name, first.name,
            "unchanged settings reuse the snapshot"
        );

        let char_dir = Path::new("Account/A#1/Realm/Char");
        assert_eq!(count_under(&first.path, char_dir), 1);
        let one_char = read(&first.path, None, Some(char_dir)).unwrap();
        assert_eq!(one_char.len(), 1);
        assert_eq!(one_char[0].1, b"d = 1");

        let only = read(&first.path, Some(&["betterbags".to_owned()]), None).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(
            only[0].0,
            Path::new("Account/A#1/SavedVariables/BetterBags.lua")
        );
        assert_eq!(only[0].1, b"bags = 1");

        std::thread::sleep(std::time::Duration::from_millis(5));
        write(
            &wtf,
            "Account/A#1/SavedVariables/BetterBags.lua",
            "bags = 22",
        );
        let second = snapshot(&wtf, &dir, "later").unwrap().unwrap();
        assert_ne!(second.name, first.name);
        let all = list(&dir);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, second.name);
        assert_eq!(all[1].reason, "before updating BetterBags");
    }
}
