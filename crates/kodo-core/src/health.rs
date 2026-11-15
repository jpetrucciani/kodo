//! Problems with what's in a client's AddOns folder that stop addons loading or working:
//! TOCs older than the game, folders WoW can't load, the same addon twice, and missing
//! required dependencies.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::local::LocalAddon;
use crate::lockfile::Lockfile;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Issue {
    /// Every TOC interface is older than the client; WoW skips it unless "Load out of date
    /// AddOns" is on.
    OutOfDate { folder: String },
    /// No TOC this client loads. `nested` names a subfolder that has one, the usual sign of a
    /// zip extracted one level too deep (`Foo-main/Foo/Foo.toc`).
    NoToc {
        folder: String,
        nested: Option<String>,
    },
    /// Folders with the same TOC title that aren't one addon kodo installed together.
    Duplicate { title: String, folders: Vec<String> },
    /// A required dependency that isn't installed.
    MissingDep { dep: String, needed_by: Vec<String> },
}

impl Issue {
    /// Folders the issue is about, for disable buttons and filtering.
    pub fn folders(&self) -> Vec<&str> {
        match self {
            Self::OutOfDate { folder } | Self::NoToc { folder, .. } => vec![folder.as_str()],
            Self::Duplicate { folders, .. } => folders.iter().map(String::as_str).collect(),
            Self::MissingDep { needed_by, .. } => needed_by.iter().map(String::as_str).collect(),
        }
    }
}

/// Check the scanned AddOns folder. Reads subfolders only for folders without a TOC.
pub fn check(addons: &Path, folders: &[LocalAddon], lock: &Lockfile) -> Vec<Issue> {
    let mut issues = Vec::new();
    for l in folders {
        if l.toc.is_none() {
            issues.push(Issue::NoToc {
                folder: l.folder.clone(),
                nested: nested_toc(&addons.join(&l.folder)),
            });
        } else if l.out_of_date {
            issues.push(Issue::OutOfDate {
                folder: l.folder.clone(),
            });
        }
    }

    let mut by_title: BTreeMap<String, Vec<&LocalAddon>> = BTreeMap::new();
    for l in folders {
        if let Some(title) = l.toc.as_ref().and_then(|t| t.title.as_deref()) {
            let key = title.trim().to_lowercase();
            if !key.is_empty() {
                by_title.entry(key).or_default().push(l);
            }
        }
    }
    for group in by_title.into_values().filter(|g| g.len() > 1) {
        let owners: Vec<_> = group.iter().map(|l| lock.owner_of(&l.folder)).collect();
        let one_addon = owners
            .iter()
            .all(|o| o.is_some() && o.map(|e| &e.id) == owners[0].map(|e| &e.id));
        if !one_addon {
            issues.push(Issue::Duplicate {
                title: group[0]
                    .toc
                    .as_ref()
                    .and_then(|t| t.title.clone())
                    .unwrap_or_default(),
                folders: group.iter().map(|l| l.folder.clone()).collect(),
            });
        }
    }

    let mut missing: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for l in folders {
        for dep in l.toc.iter().flat_map(|t| &t.dependencies) {
            if is_builtin(dep) || has_folder(folders, dep) {
                continue;
            }
            missing
                .entry(dep.to_lowercase())
                .or_insert_with(|| (dep.clone(), Vec::new()))
                .1
                .push(l.folder.clone());
        }
    }
    issues.extend(
        missing
            .into_values()
            .map(|(dep, needed_by)| Issue::MissingDep { dep, needed_by }),
    );
    issues
}

/// Installed folders (outside `removing`) that list one of `removing` as a required dependency.
pub fn dependents(folders: &[LocalAddon], removing: &[String]) -> Vec<String> {
    let gone = |f: &str| removing.iter().any(|r| r.eq_ignore_ascii_case(f));
    folders
        .iter()
        .filter(|l| !gone(&l.folder))
        .filter(|l| {
            l.toc
                .as_ref()
                .is_some_and(|t| t.dependencies.iter().any(|d| gone(d)))
        })
        .map(|l| l.folder.clone())
        .collect()
}

/// Plan removing several addons at once, each given as its folders. Returns the order to
/// remove them in (dependents before what they need) and the ones to keep, with the folders
/// outside the removal that still need them. Keeping one keeps what it needs, too.
pub fn removal_order(
    folders: &[LocalAddon],
    groups: &[Vec<String>],
) -> (Vec<usize>, Vec<(usize, Vec<String>)>) {
    let needs: Vec<Vec<String>> = groups.iter().map(|g| dependents(folders, g)).collect();
    let mut kept: Vec<Option<Vec<String>>> = vec![None; groups.len()];
    loop {
        let going: Vec<&String> = groups
            .iter()
            .zip(&kept)
            .filter(|(_, k)| k.is_none())
            .flat_map(|(g, _)| g)
            .collect();
        let mut changed = false;
        for (i, need) in needs.iter().enumerate() {
            if kept[i].is_some() {
                continue;
            }
            let outside: Vec<String> = need
                .iter()
                .filter(|f| !going.iter().any(|g| g.eq_ignore_ascii_case(f)))
                .cloned()
                .collect();
            if !outside.is_empty() {
                kept[i] = Some(outside);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut left: Vec<usize> = (0..groups.len()).filter(|&i| kept[i].is_none()).collect();
    let mut order = Vec::with_capacity(left.len());
    while !left.is_empty() {
        // One that no other remaining addon needs; in a dependency cycle, just the first.
        let pos = left
            .iter()
            .position(|&i| {
                !left.iter().any(|&j| {
                    j != i
                        && groups[j]
                            .iter()
                            .any(|f| needs[i].iter().any(|n| n.eq_ignore_ascii_case(f)))
                })
            })
            .unwrap_or(0);
        order.push(left.remove(pos));
    }
    let kept = kept
        .into_iter()
        .enumerate()
        .filter_map(|(i, k)| k.map(|by| (i, by)))
        .collect();
    (order, kept)
}

/// Standalone library folders nothing installed lists as a dependency (required or optional),
/// so probably left over from an addon that's gone. Folders shipped with a multi-folder addon
/// kodo manages are left alone; they belong to that addon.
pub fn unused_libraries(folders: &[LocalAddon], lock: &Lockfile) -> Vec<String> {
    let wanted = |name: &str| {
        folders.iter().any(|l| {
            !l.folder.eq_ignore_ascii_case(name)
                && l.toc.as_ref().is_some_and(|t| {
                    t.dependencies
                        .iter()
                        .chain(&t.optional_deps)
                        .any(|d| d.eq_ignore_ascii_case(name))
                })
        })
    };
    folders
        .iter()
        .filter(|l| is_library(l))
        .filter(|l| {
            lock.owner_of(&l.folder)
                .is_none_or(|e| e.folders.len() == 1)
        })
        .filter(|l| !wanted(&l.folder))
        .map(|l| l.folder.clone())
        .collect()
}

/// `Lib*` folders, Ace3, and anything whose TOC files it under libraries.
fn is_library(l: &LocalAddon) -> bool {
    let f = l.folder.as_str();
    let lib_name = f.len() > 3
        && f.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("lib"))
        && f[3..].starts_with(|c: char| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-');
    let category = l.toc.as_ref().is_some_and(|t| {
        t.extra.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("X-Category") && v.to_ascii_lowercase().contains("librar")
        })
    });
    lib_name || f.eq_ignore_ascii_case("Ace3") || category
}

/// Blizzard's own `Blizzard_*` modules ship with the game.
fn is_builtin(dep: &str) -> bool {
    dep.get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("Blizzard_"))
}

fn has_folder(folders: &[LocalAddon], name: &str) -> bool {
    folders.iter().any(|l| l.folder.eq_ignore_ascii_case(name))
}

/// First subfolder holding a `.toc`, if any.
fn nested_toc(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let path = e.path();
        let has_toc = std::fs::read_dir(&path).ok()?.flatten().any(|f| {
            f.file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".toc")
        });
        has_toc.then(|| e.file_name().to_string_lossy().into_owned())
    })
}

#[cfg(test)]
mod tests {
    use crate::flavor::Flavor;
    use crate::health::{Issue, check, dependents, removal_order, unused_libraries};
    use crate::local::LocalAddon;
    use crate::lockfile::{LockEntry, Lockfile};
    use crate::toc::Toc;

    fn folder(name: &str, toc: Option<&str>) -> LocalAddon {
        LocalAddon {
            folder: name.into(),
            toc_file: toc.map(|_| format!("{name}.toc")),
            toc: toc.map(Toc::parse),
            out_of_date: false,
        }
    }

    #[test]
    fn finds_load_problems_and_dependents() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("Foo-main/Foo")).unwrap();
        std::fs::write(tmp.path().join("Foo-main/Foo/Foo.toc"), "").unwrap();
        let mut old = folder("Old", Some("## Title: Old"));
        old.out_of_date = true;
        let folders = vec![
            folder(
                "Bags",
                Some("## Title: Bags\n## Dependencies: LibDD, Blizzard_Bags"),
            ),
            folder("Bags2", Some("## Title: Bags")),
            folder("DBM-Core", Some("## Title: DBM")),
            folder("DBM-PvP", Some("## Title: DBM\n## RequiredDeps: DBM-Core")),
            folder("Foo-main", None),
            old,
        ];
        let mut lock = Lockfile::new(Flavor::Forever, tmp.path().into());
        lock.upsert(LockEntry::new(
            "github:x/DBM".parse().unwrap(),
            "1",
            vec!["DBM-Core".into(), "DBM-PvP".into()],
        ));
        let issues = check(tmp.path(), &folders, &lock);
        assert!(issues.contains(&Issue::NoToc {
            folder: "Foo-main".into(),
            nested: Some("Foo".into())
        }));
        assert!(issues.contains(&Issue::OutOfDate {
            folder: "Old".into()
        }));
        assert!(issues.contains(&Issue::Duplicate {
            title: "Bags".into(),
            folders: vec!["Bags".into(), "Bags2".into()]
        }));
        assert!(issues.contains(&Issue::MissingDep {
            dep: "LibDD".into(),
            needed_by: vec!["Bags".into()]
        }));
        // DBM's two folders are one addon, and its dependency is installed.
        assert_eq!(issues.len(), 4, "{issues:?}");

        assert_eq!(dependents(&folders, &["dbm-core".into()]), ["DBM-PvP"]);
        assert!(dependents(&folders, &["DBM-Core".into(), "DBM-PvP".into()]).is_empty());
    }

    #[test]
    fn finds_unused_libraries() {
        let tmp = tempfile::tempdir().unwrap();
        let folders = vec![
            folder("LibStub", Some("## Title: LibStub")),
            folder("LibDBIcon-1.0", Some("## Title: LibDBIcon")),
            folder("Ace3", Some("## Title: Ace3")),
            folder("Library", Some("## Title: Not a lib, just the name")),
            folder(
                "Bags",
                Some("## Title: Bags\n## OptionalDeps: libstub, Ace3"),
            ),
        ];
        let lock = Lockfile::new(Flavor::Forever, tmp.path().into());
        assert_eq!(unused_libraries(&folders, &lock), ["LibDBIcon-1.0"]);
    }

    #[test]
    fn removes_dependents_first_and_keeps_what_outsiders_need() {
        let folders = vec![
            folder("Lib", Some("## Title: Lib")),
            folder("Core", Some("## Title: Core\n## Dependencies: Lib")),
            folder("Plugin", Some("## Title: Plugin\n## Dependencies: Core")),
            folder("Other", Some("## Title: Other\n## Dependencies: Plugin")),
        ];
        let g = |f: &str| vec![f.to_owned()];
        // Lib is listed first but goes last; nothing outside needs any of them.
        let (order, kept) = removal_order(&folders[..3], &[g("Lib"), g("Core"), g("Plugin")]);
        assert_eq!(order, [2, 1, 0]);
        assert!(kept.is_empty());
        // Other still needs Plugin, so Plugin stays, and so do Core and Lib under it.
        let (order, kept) = removal_order(&folders, &[g("Lib"), g("Core"), g("Plugin")]);
        assert!(order.is_empty(), "{order:?}");
        assert_eq!(kept[0], (0, vec!["Core".to_owned()]));
        assert_eq!(kept[2], (2, vec!["Other".to_owned()]));
    }
}
