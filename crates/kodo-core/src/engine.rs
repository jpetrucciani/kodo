//! Everything a frontend can do, as async operations over injected sources.
//!
//! Downloads are bounded by a semaphore (the job queue), and every disk change for a client
//! runs under one mutex on a blocking thread, so the GUI and CLI get identical behaviour.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use crate::addon::{AddonRef, Change, Channel, Match, Query, Release, RemoteAddon, SourceId};
use crate::characters::{self, Character};
use crate::config::Paths;
use crate::detect::Client;
use crate::flavor::Flavor;
use crate::health::{self, Issue};
use crate::history::{self, Event, Kind, Undo};
use crate::import::Wanted;
use crate::install::{self, BACKUP_ENTRY};
use crate::local::{self, LocalAddon};
use crate::lockfile::{LockEntry, Lockfile, now_rfc3339};
use crate::profile::{Profile, Profiles};
use crate::source::{Progress, Source};
use crate::wtf::{self, Snapshot};
use crate::{Error, Result};

/// Backups kept per addon after an install (spec step 7).
const KEEP_BACKUPS: usize = 2;

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    paths: Paths,
    sources: Vec<Arc<dyn Source>>,
    downloads: Semaphore,
    disk: Mutex<()>,
}

#[derive(Clone, Default)]
pub struct InstallOptions {
    pub channel: Channel,
    /// Install this exact version and pin it.
    pub version: Option<String>,
    /// Take over existing folders kodo doesn't manage.
    pub force: bool,
    pub progress: Option<Progress>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Installed {
    pub entry: LockEntry,
    pub previous: Option<String>,
    /// Non-fatal problem, e.g. the old version could not be backed up.
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheck {
    pub entry: LockEntry,
    pub latest: Option<Release>,
    pub error: Option<String>,
}

impl UpdateCheck {
    pub fn has_update(&self) -> bool {
        !self.entry.pinned
            && self
                .latest
                .as_ref()
                .is_some_and(|r| r.version != self.entry.version)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchResults {
    pub hits: Vec<RemoteAddon>,
    pub errors: Vec<(SourceId, String)>,
}

/// A rollback, and how many saved-settings files came back with it.
#[derive(Debug, Clone, Serialize)]
pub struct RolledBack {
    pub entry: LockEntry,
    pub settings: usize,
    pub warning: Option<String>,
}

/// What a profile switch would do, for a preview before it runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SwitchPlan {
    pub profile: String,
    /// Installed addons the profile doesn't list; they move to the stash.
    pub stash: Vec<String>,
    /// Listed addons waiting in the stash.
    pub restore: Vec<String>,
    /// Listed addons kodo has nowhere; they're downloaded.
    pub download: Vec<String>,
}

/// What a profile switch did. Failures don't stop the switch; the rest still applies.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SwitchReport {
    pub profile: String,
    /// Moved out of AddOns into the stash.
    pub stashed: Vec<String>,
    /// Moved back from the stash.
    pub restored: Vec<String>,
    /// Downloaded because nothing was stashed for them.
    pub installed: Vec<String>,
    pub failed: Vec<(String, String)>,
    /// Non-fatal problem, e.g. saved settings could not be backed up first.
    pub warning: Option<String>,
    /// The profile that was active before, to switch back to.
    pub from: Option<String>,
}

/// A character and its addon switches as AddOns.txt lists them (see [`crate::characters`]).
#[derive(Debug, Clone, Serialize)]
pub struct CharacterAddons {
    pub character: Character,
    pub states: BTreeMap<String, bool>,
    /// Unix seconds of its last logout.
    pub last_played: Option<u64>,
    /// Addon folders (lowercased) with settings saved just for this character.
    pub saved: BTreeSet<String>,
    /// Settings snapshots holding this character's files, newest first.
    pub snapshots: Vec<CharacterSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CharacterSnapshot {
    pub name: String,
    pub made: u64,
    pub reason: String,
    /// This character's files in it.
    pub files: usize,
}

/// A zip installed by hand: its folders stay unmanaged until linked to a source.
#[derive(Debug, Clone, Serialize)]
pub struct ZipInstalled {
    pub folders: Vec<String>,
    /// Hand-installed folders it replaced (moved to kodo's backups).
    pub replaced: Vec<String>,
}

/// An entry of an imported list, matched against the sources.
#[derive(Debug, Clone, Serialize)]
pub struct ImportMatch {
    pub wanted: Wanted,
    /// The project to install: the list's own ref, else the best search hit.
    pub addon: Option<AddonRef>,
    /// Display name of the match.
    pub name: String,
    /// False when a name search found only a near miss; ask before installing it.
    pub exact: bool,
    pub installed: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanReport {
    /// Entries newly written to the lockfile by this scan.
    pub adopted: Vec<LockEntry>,
    /// Entries that were already managed and are present on disk.
    pub managed: Vec<LockEntry>,
    /// Managed entries whose folders are all gone (deleted by hand).
    pub missing: Vec<LockEntry>,
    /// Folders nothing matched. kodo never touches these.
    pub local: Vec<LocalAddon>,
    /// Name-based guesses for `local` folders; the user confirms by installing over them.
    pub suggestions: Vec<Match>,
    pub errors: Vec<(SourceId, String)>,
}

impl Engine {
    pub fn new(paths: Paths, sources: Vec<Arc<dyn Source>>, concurrency: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                paths,
                sources,
                downloads: Semaphore::new(concurrency.max(1)),
                disk: Mutex::new(()),
            }),
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.inner.paths
    }

    pub fn source_ids(&self) -> Vec<SourceId> {
        self.inner.sources.iter().map(|s| s.id()).collect()
    }

    fn source(&self, id: SourceId) -> Result<Arc<dyn Source>> {
        self.inner
            .sources
            .iter()
            .find(|s| s.id() == id)
            .cloned()
            .ok_or(Error::SourceDisabled(id))
    }

    pub fn lockfile(&self, client: &Client) -> Result<Lockfile> {
        Lockfile::load_or_new(
            &self.paths().lockfile(client),
            client.flavor.clone(),
            &client.addons,
        )
    }

    /// Search every enabled source (or one) concurrently. One failing source doesn't fail the rest.
    /// Search (or, with empty text, browse) every enabled source, or just `only`.
    pub async fn search(
        &self,
        q: &Query,
        flavor: &Flavor,
        only: Option<SourceId>,
    ) -> SearchResults {
        let mut set = JoinSet::new();
        for src in self
            .inner
            .sources
            .iter()
            .filter(|s| only.is_none_or(|o| o == s.id()))
        {
            let (src, flavor, q) = (src.clone(), flavor.clone(), q.clone());
            set.spawn(async move { (src.id(), src.search(&q, &flavor).await) });
        }
        let mut out = SearchResults::default();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((_, Ok(hits))) => out.hits.extend(hits),
                Ok((id, Err(e))) => out.errors.push((id, e.to_string())),
                Err(e) => out.errors.push((SourceId::Github, e.to_string())),
            }
        }
        q.rank(&mut out.hits);
        out
    }

    pub async fn resolve(
        &self,
        client: &Client,
        addon: &AddonRef,
        channel: Channel,
        version: Option<&str>,
    ) -> Result<Release> {
        let src = self.source(addon.source)?;
        Ok(match version {
            Some(v) => src.resolve_version(addon, &client.flavor, v).await?,
            None => src.resolve(addon, &client.flavor, channel).await?,
        })
    }

    pub async fn readme(&self, addon: &AddonRef) -> Result<Option<String>> {
        Ok(self.source(addon.source)?.readme(addon).await?)
    }

    /// Release notes from the installed version up to the one a check found, newest first.
    /// Sources that only describe their latest release give just that one.
    pub async fn changes(&self, check: &UpdateCheck) -> Result<Vec<Change>> {
        let e = &check.entry;
        let listed = self
            .source(e.id.source)?
            .changes(&e.id, e.channel, &e.version)
            .await?;
        Ok(match listed.filter(|l| !l.is_empty()) {
            Some(list) => list,
            None => check.latest.iter().map(Change::from).collect(),
        })
    }

    pub async fn install(
        &self,
        client: &Client,
        addon: &AddonRef,
        opts: InstallOptions,
    ) -> Result<Installed> {
        let release = self
            .resolve(client, addon, opts.channel, opts.version.as_deref())
            .await?;
        let pinned = opts.version.is_some();
        self.install_release(
            client,
            release,
            opts.channel,
            pinned,
            opts.force,
            opts.progress,
        )
        .await
    }

    /// Install a release resolved earlier (e.g. by [`Engine::check_updates`]).
    pub async fn install_release(
        &self,
        client: &Client,
        release: Release,
        channel: Channel,
        pinned: bool,
        force: bool,
        progress: Option<Progress>,
    ) -> Result<Installed> {
        let url = release
            .download_url
            .clone()
            .ok_or_else(|| Error::Unavailable {
                name: release.name.clone(),
                page: release.page_url.clone().unwrap_or_default(),
            })?;
        let zip = {
            let _permit = self
                .inner
                .downloads
                .acquire()
                .await
                .map_err(|e| Error::Task(e.to_string()))?;
            self.fetch(&release, &url, progress).await?
        };
        let this = self.clone();
        let c = client.clone();
        let done = self
            .blocking(move || this.apply(&c, &release, &url, &zip, channel, pinned, force))
            .await?;
        let name = done.entry.display_name().to_owned();
        let event = match &done.previous {
            Some(prev) if *prev != done.entry.version => Event::now(
                Kind::Update,
                format!("updated {name} {prev} → {}", done.entry.version),
                Some(Undo::Rollback {
                    addon: done.entry.id.clone(),
                }),
            ),
            Some(_) => Event::now(
                Kind::Install,
                format!("reinstalled {name} {}", done.entry.version),
                None,
            ),
            None => Event::now(
                Kind::Install,
                format!("installed {name} {}", done.entry.version),
                Some(Undo::Remove {
                    addon: done.entry.id.clone(),
                }),
            ),
        };
        self.log(client, &event);
        Ok(done)
    }

    /// Download to the cache, keyed by URL and size, reusing a previous download.
    async fn fetch(
        &self,
        release: &Release,
        url: &str,
        progress: Option<Progress>,
    ) -> Result<PathBuf> {
        let size = release
            .size
            .map_or_else(|| "x".to_owned(), |s| s.to_string());
        let path = self
            .paths()
            .downloads()
            .join(format!("{:016x}-{size}.zip", crate::fnv1a(url.as_bytes())));
        let cached = std::fs::metadata(&path)
            .ok()
            .filter(|m| release.size.is_none_or(|s| s == m.len()));
        if cached.is_none() {
            let dir = self.paths().downloads();
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(crate::io_err("create", &dir))?;
            self.source(release.addon.source)?
                .download(url, &path, progress)
                .await?;
        }
        Ok(path)
    }

    #[allow(clippy::too_many_arguments)]
    fn apply(
        &self,
        client: &Client,
        release: &Release,
        url: &str,
        zip: &Path,
        channel: Channel,
        pinned: bool,
        force: bool,
    ) -> Result<Installed> {
        ensure_closed(client)?;
        let lock_path = self.paths().lockfile(client);
        let backups = self.paths().backups(client);
        let mut lock = self.lockfile(client)?;
        let previous = lock.get(&release.addon).cloned();

        let staged = install::extract(zip, release.size, &client.addons)?;
        let disabled = install::disabled(&client.addons);
        let parked = staged
            .folders
            .iter()
            .chain(previous.iter().flat_map(|p| &p.folders))
            .any(|f| disabled.iter().any(|d| d.eq_ignore_ascii_case(f)));
        if parked {
            return Err(Error::Disabled(release.name.clone()));
        }
        for f in &staged.folders {
            match lock.owner_of(f) {
                Some(owner) if owner.id != release.addon => {
                    return Err(Error::Conflict {
                        folder: f.clone(),
                        owner: owner.id.to_string(),
                    });
                }
                Some(_) => {}
                // An addon kodo already manages may grow modules that were installed by hand.
                None if client.addons.join(f).exists() && previous.is_none() && !force => {
                    return Err(Error::Unmanaged(f.clone()));
                }
                None => {}
            }
        }

        let mut old: Vec<String> = previous
            .as_ref()
            .map(|p| p.folders.clone())
            .unwrap_or_default();
        for f in &staged.folders {
            if client.addons.join(f).exists() && !old.iter().any(|o| o.eq_ignore_ascii_case(f)) {
                old.push(f.clone());
            }
        }
        let old_entry = previous
            .clone()
            .unwrap_or_else(|| LockEntry::new(release.addon.clone(), "unmanaged", old.clone()));
        let (settings, settings_warning) = if old.is_empty() {
            (None, None)
        } else {
            self.snapshot_wtf(client, &format!("before updating {}", release.name))
        };
        let backup = install::backup_dir(&backups, &old_entry);
        let swapped = install::swap(
            &client.addons,
            staged.path(),
            &staged.folders,
            &old,
            &backup,
        )?;
        let mut warning = swapped.backup_warning;
        if !old.is_empty() && warning.is_none() {
            let mut recorded = old_entry;
            recorded.folders = old;
            if let Err(e) = install::write_backup_entry(&backup, &recorded, settings.as_deref()) {
                warning = Some(e.to_string());
            }
        }
        warning = warning.or(settings_warning);

        let entry = LockEntry {
            name: Some(release.name.clone()),
            channel,
            pinned,
            folders: staged.folders.clone(),
            asset_url: Some(url.to_owned()),
            sha256: Some(install::sha256_file(zip)?),
            ..LockEntry::new(release.addon.clone(), release.version.clone(), Vec::new())
        };
        lock.upsert(entry.clone());
        lock.save(&lock_path)?;
        if let Err(e) = install::prune_backups(&backups, &entry, KEEP_BACKUPS) {
            warning.get_or_insert(e.to_string());
        }
        Ok(Installed {
            entry,
            previous: previous.map(|p| p.version),
            warning,
        })
    }

    /// Resolve the newest release for every managed addon, concurrently.
    pub async fn check_updates(&self, client: &Client) -> Result<Vec<UpdateCheck>> {
        let lock = self.lockfile(client)?;
        let mut set = JoinSet::new();
        for (i, entry) in lock.addons.into_iter().enumerate() {
            let (this, client) = (self.clone(), client.clone());
            set.spawn(async move {
                if entry.pinned {
                    return (
                        i,
                        UpdateCheck {
                            entry,
                            latest: None,
                            error: None,
                        },
                    );
                }
                let res = this.resolve(&client, &entry.id, entry.channel, None).await;
                let (latest, error) = match res {
                    Ok(r) => (Some(r), None),
                    Err(e) => (None, Some(e.to_string())),
                };
                (
                    i,
                    UpdateCheck {
                        entry,
                        latest,
                        error,
                    },
                )
            });
        }
        let mut out = Vec::new();
        while let Some(joined) = set.join_next().await {
            out.push(joined.map_err(|e| Error::Task(e.to_string()))?);
        }
        out.sort_by_key(|(i, _)| *i);
        Ok(out.into_iter().map(|(_, c)| c).collect())
    }

    /// Install the newer release found by a check, keeping the entry's channel.
    pub async fn apply_update(
        &self,
        client: &Client,
        check: &UpdateCheck,
        progress: Option<Progress>,
    ) -> Result<Installed> {
        if check.entry.pinned {
            return Err(Error::Pinned(check.entry.display_name().to_owned()));
        }
        let release = check
            .latest
            .clone()
            .ok_or_else(|| Error::NotInstalled(check.entry.id.to_string()))?;
        self.install_release(client, release, check.entry.channel, false, false, progress)
            .await
    }

    /// Pin the installed version, or install and pin a specific one.
    pub async fn pin(
        &self,
        client: &Client,
        name: &str,
        version: Option<&str>,
    ) -> Result<LockEntry> {
        let entry = self.lockfile(client)?.find(name)?.clone();
        if let Some(v) = version {
            let release = self
                .resolve(client, &entry.id, entry.channel, Some(v))
                .await?;
            return Ok(self
                .install_release(client, release, entry.channel, true, false, None)
                .await?
                .entry);
        }
        self.edit_entry(client, name, |e| e.pinned = true).await
    }

    pub async fn unpin(&self, client: &Client, name: &str) -> Result<LockEntry> {
        self.edit_entry(client, name, |e| e.pinned = false).await
    }

    pub async fn set_channel(
        &self,
        client: &Client,
        name: &str,
        channel: Channel,
    ) -> Result<LockEntry> {
        self.edit_entry(client, name, move |e| e.channel = channel)
            .await
    }

    async fn edit_entry(
        &self,
        client: &Client,
        name: &str,
        edit: impl FnOnce(&mut LockEntry) + Send + 'static,
    ) -> Result<LockEntry> {
        let (this, client, name) = (self.clone(), client.clone(), name.to_owned());
        self.blocking(move || {
            let mut lock = this.lockfile(&client)?;
            let mut entry = lock.find(&name)?.clone();
            edit(&mut entry);
            lock.upsert(entry.clone());
            lock.save(&this.paths().lockfile(&client))?;
            Ok(entry)
        })
        .await
    }

    /// Move an addon's folders to its backup dir and forget it. Undo with [`Engine::rollback`].
    /// Refuses when another installed addon requires one of its folders, unless `force`.
    pub async fn remove(&self, client: &Client, name: &str, force: bool) -> Result<LockEntry> {
        let (this, client, name) = (self.clone(), client.clone(), name.to_owned());
        self.blocking(move || {
            ensure_closed(&client)?;
            let mut lock = this.lockfile(&client)?;
            let entry = lock.find(&name)?.clone();
            let disabled = install::disabled(&client.addons);
            if entry.folders.iter().any(|f| disabled.contains(f)) {
                return Err(Error::Disabled(entry.display_name().to_owned()));
            }
            if !force && client.addons.is_dir() {
                let needs = health::dependents(&local::scan(&client)?, &entry.folders);
                if !needs.is_empty() {
                    return Err(Error::Needed {
                        name: entry.display_name().to_owned(),
                        by: needs.join(", "),
                    });
                }
            }
            let (settings, _) = this.snapshot_wtf(
                &client,
                &format!("before removing {}", entry.display_name()),
            );
            let backup = install::backup_dir(&this.paths().backups(&client), &entry);
            let swapped =
                install::swap(&client.addons, &client.addons, &[], &entry.folders, &backup)?;
            if swapped.backup_warning.is_none() && backup.exists() {
                install::write_backup_entry(&backup, &entry, settings.as_deref())?;
            }
            lock.remove(&entry.id);
            lock.save(&this.paths().lockfile(&client))?;
            this.log(
                &client,
                &Event::now(
                    Kind::Remove,
                    format!("removed {}", entry.display_name()),
                    Some(Undo::Rollback {
                        addon: entry.id.clone(),
                    }),
                ),
            );
            Ok(entry)
        })
        .await
    }

    /// Remove several addons. One is refused only when an addon outside the set needs it (or
    /// something kept that it needs), unless `force`; dependents go before what they need, so
    /// a library and the addons using it can go together. Results are per name, in removal
    /// order, with refused ones last.
    pub async fn remove_many(
        &self,
        client: &Client,
        names: &[String],
        force: bool,
    ) -> Result<Vec<(String, Result<LockEntry>)>> {
        let (this, c, list) = (self.clone(), client.clone(), names.to_vec());
        let (mut order, kept) = self
            .blocking(move || {
                let lock = this.lockfile(&c)?;
                let groups: Vec<Vec<String>> = list
                    .iter()
                    .map(|n| lock.find(n).map(|e| e.folders.clone()).unwrap_or_default())
                    .collect();
                let local = if c.addons.is_dir() {
                    local::scan(&c)?
                } else {
                    Vec::new()
                };
                Ok(health::removal_order(&local, &groups))
            })
            .await?;
        let mut out = Vec::with_capacity(names.len());
        let mut refused = Vec::new();
        for (i, by) in kept {
            if force {
                order.push(i);
            } else {
                refused.push((
                    names[i].clone(),
                    Err(Error::Needed {
                        name: names[i].clone(),
                        by: by.join(", "),
                    }),
                ));
            }
        }
        for i in order {
            // Checked against the whole set above.
            out.push((names[i].clone(), self.remove(client, &names[i], true).await));
        }
        out.extend(refused);
        Ok(out)
    }

    /// Restore the newest backup of another version, or bring back a removed addon, along with
    /// the saved settings it had then (current settings are snapshotted first).
    pub async fn rollback(&self, client: &Client, name: &str) -> Result<RolledBack> {
        let (this, client, name) = (self.clone(), client.clone(), name.to_owned());
        self.blocking(move || {
            ensure_closed(&client)?;
            let backups = this.paths().backups(&client);
            let mut lock = this.lockfile(&client)?;
            let current = match lock.find(&name) {
                Ok(e) => Some(e.clone()),
                Err(Error::NotInstalled(_)) => None,
                Err(e) => return Err(e),
            };
            let candidates = match &current {
                Some(cur) => install::list_backups(&backups, cur)
                    .into_iter()
                    .filter(|(_, b)| b.version != cur.version)
                    .collect::<Vec<_>>(),
                None => install::all_backups(&backups)
                    .into_iter()
                    .filter(|(_, b)| b.answers_to(&name) && lock.get(&b.id).is_none())
                    .collect(),
            };
            let Some((dir, mut restored)) = candidates.into_iter().next() else {
                return Err(Error::NoBackup(name));
            };
            // Read the old settings before the safety snapshot below can prune their zip.
            let wtf_dir = this.paths().wtf_backups(&client);
            let saved = install::backup_wtf(&dir)
                .and_then(|n| wtf::find(&wtf_dir, &n))
                .map(|s| wtf::read(&s.path, Some(&restored.folders), None));
            let (current_settings, mut warning) = this.snapshot_wtf(
                &client,
                &format!("before rolling back {}", restored.display_name()),
            );
            let staged = install::stage_backup(&dir, &restored.folders, &client.addons)?;
            let old = current
                .as_ref()
                .map(|c| c.folders.clone())
                .unwrap_or_default();
            let current_backup = current
                .as_ref()
                .map(|c| install::backup_dir(&backups, c))
                .unwrap_or_else(|| dir.with_extension("replaced"));
            let swapped = install::swap(
                &client.addons,
                staged.path(),
                &staged.folders,
                &old,
                &current_backup,
            )?;
            if let (Some(cur), None) = (&current, &swapped.backup_warning)
                && current_backup.exists()
            {
                install::write_backup_entry(&current_backup, cur, current_settings.as_deref())?;
            }
            let _ = std::fs::remove_dir_all(&dir);
            restored.installed_at = now_rfc3339();
            lock.upsert(restored.clone());
            lock.save(&this.paths().lockfile(&client))?;

            // Only overwrite settings when the current ones are safely snapshotted.
            let mut settings = 0;
            match saved {
                Some(Ok(files)) if warning.is_none() => {
                    match install::restore_files(&client.wtf(), &files) {
                        Ok(()) => settings = files.len(),
                        Err(e) => warning = Some(format!("saved settings not restored: {e}")),
                    }
                }
                Some(Err(e)) => warning = Some(format!("saved settings not restored: {e}")),
                _ => {}
            }
            // The newest backup of another version is the one just replaced, so rolling back
            // again undoes this.
            this.log(
                &client,
                &Event::now(
                    Kind::Rollback,
                    format!(
                        "rolled back {} to {}",
                        restored.display_name(),
                        restored.version
                    ),
                    Some(Undo::Rollback {
                        addon: restored.id.clone(),
                    }),
                ),
            );
            Ok(RolledBack {
                entry: restored,
                settings,
                warning,
            })
        })
        .await
    }

    /// Match installed folders to sources and adopt confident matches into the lockfile.
    ///
    /// Order: lockfile, then TOC headers (`X-Website` GitHub URL, `X-WoWI-ID`,
    /// `X-Curse-Project-ID`, `X-Wago-ID`) for enabled sources, then source matchers such as
    /// CurseForge fingerprints. Fuzzy name matches are never adopted.
    pub async fn scan(&self, client: &Client, write: bool) -> Result<ScanReport> {
        let (this, c) = (self.clone(), client.clone());
        let (lock, folders) = self
            .blocking(move || Ok((this.lockfile(&c)?, local::scan(&c)?)))
            .await?;

        let mut report = ScanReport::default();
        for entry in &lock.addons {
            if entry
                .folders
                .iter()
                .any(|f| folders.iter().any(|l| l.folder.eq_ignore_ascii_case(f)))
            {
                report.managed.push(entry.clone());
            } else {
                report.missing.push(entry.clone());
            }
        }

        let mut adopted: Vec<LockEntry> = Vec::new();
        let mut unmatched = Vec::new();
        for addon in folders
            .into_iter()
            .filter(|l| lock.owner_of(&l.folder).is_none())
        {
            let hit = addon
                .toc
                .as_ref()
                .map(header_refs)
                .unwrap_or_default()
                .into_iter()
                .find(|r| self.source(r.source).is_ok());
            match hit {
                Some(r) => match adopted.iter_mut().find(|e| e.id == r) {
                    Some(e) => {
                        // DBM-Core + DBM-PvP is one addon; unrelated folders from a monorepo
                        // are a bundle, so name the entry after the repo, not its first folder.
                        if e.folders.first().map(|f| stem(f)) != Some(stem(&addon.folder)) {
                            e.name = None;
                        }
                        e.folders.push(addon.folder);
                    }
                    None => {
                        let toc = addon.toc.as_ref();
                        let version = toc
                            .and_then(|t| t.version.clone())
                            .unwrap_or_else(|| "unknown".into());
                        let mut entry = LockEntry::new(r, version, vec![addon.folder.clone()]);
                        entry.name = toc.and_then(|t| t.title.clone());
                        adopted.push(entry);
                    }
                },
                None => unmatched.push(addon),
            }
        }

        for src in &self.inner.sources {
            if unmatched.is_empty() {
                break;
            }
            match src.match_installed(&unmatched).await {
                Ok(matches) => {
                    for m in matches {
                        unmatched.retain(|l| {
                            !m.folders.iter().any(|f| f.eq_ignore_ascii_case(&l.folder))
                        });
                        let mut entry = LockEntry::new(
                            m.addon,
                            m.version.unwrap_or_else(|| "unknown".into()),
                            m.folders,
                        );
                        entry.name = None;
                        adopted.push(entry);
                    }
                }
                Err(e) => report.errors.push((src.id(), e.to_string())),
            }
        }

        for src in &self.inner.sources {
            if unmatched.is_empty() {
                break;
            }
            match src.suggest(&unmatched, &client.flavor).await {
                Ok(found) => {
                    for m in found {
                        let taken = report
                            .suggestions
                            .iter()
                            .any(|s| s.folders.iter().any(|f| m.folders.contains(f)));
                        if !taken {
                            report.suggestions.push(m);
                        }
                    }
                }
                Err(e) => report.errors.push((src.id(), e.to_string())),
            }
        }

        if write && !adopted.is_empty() {
            let (this, c, new) = (self.clone(), client.clone(), adopted.clone());
            self.blocking(move || {
                let mut lock = this.lockfile(&c)?;
                for e in new {
                    lock.upsert(e);
                }
                lock.save(&this.paths().lockfile(&c))
            })
            .await?;
        }
        report.adopted = adopted;
        report.local = unmatched;
        Ok(report)
    }

    /// The client's profiles. One is always active and lists exactly what's installed; the
    /// first call on a client saves the current addons as "Default".
    pub fn profiles(&self, client: &Client) -> Result<Profiles> {
        let path = self.paths().profiles(client);
        let mut set = Profiles::load(&path)?;
        if set.sync_active(&self.lockfile(client)?.addons) {
            set.save(&path)?;
        }
        Ok(set)
    }

    /// Save what's installed (or just `only`) as a profile, replacing one with the same name.
    pub async fn save_profile(
        &self,
        client: &Client,
        name: &str,
        only: Option<Vec<AddonRef>>,
    ) -> Result<Profile> {
        let (this, client, name) = (self.clone(), client.clone(), name.trim().to_owned());
        if name.is_empty() {
            return Err(Error::BadRef("a profile needs a name".into()));
        }
        self.blocking(move || {
            let path = this.paths().profiles(&client);
            let mut set = Profiles::load(&path)?;
            let lock = this.lockfile(&client)?;
            set.sync_active(&lock.addons);
            if set.is_active(&name) && only.is_some() {
                return Err(Error::BadRef(format!(
                    "{name} is the active profile, which always lists everything installed; pick another name"
                )));
            }
            let profile = Profile::from_entries(&name, &lock.addons, only.as_deref());
            set.upsert(profile.clone());
            set.save(&path)?;
            Ok(profile)
        })
        .await
    }

    /// Store an imported profile under a free name; returns the stored profile.
    pub async fn add_profile(&self, client: &Client, mut profile: Profile) -> Result<Profile> {
        let (this, client) = (self.clone(), client.clone());
        self.blocking(move || {
            let path = this.paths().profiles(&client);
            let mut set = Profiles::load(&path)?;
            profile.name = set.unique_name(&profile.name);
            set.upsert(profile.clone());
            set.save(&path)?;
            Ok(profile)
        })
        .await
    }

    /// Forget an inactive profile. Its stashed addons stay stashed until another profile lists
    /// them.
    pub async fn delete_profile(&self, client: &Client, name: &str) -> Result<Profile> {
        let (this, client, name) = (self.clone(), client.clone(), name.to_owned());
        self.blocking(move || {
            let path = this.paths().profiles(&client);
            let mut set = Profiles::load(&path)?;
            if set.is_active(&name) {
                return Err(Error::BadRef(format!(
                    "{name} is the active profile; switch to another one before deleting it"
                )));
            }
            let gone = set.remove(&name).ok_or(Error::NoProfile(name))?;
            set.save(&path)?;
            Ok(gone)
        })
        .await
    }

    /// What switching to `name` would stash, restore and download. Reads only.
    pub fn plan_switch(&self, client: &Client, name: &str) -> Result<SwitchPlan> {
        let set = self.profiles(client)?;
        let target = set
            .get(name)
            .ok_or_else(|| Error::NoProfile(name.to_owned()))?;
        let lock = self.lockfile(client)?;
        let stashed = install::stashed(&client.addons);
        let mut plan = SwitchPlan {
            profile: target.name.clone(),
            stash: lock
                .addons
                .iter()
                .filter(|e| !target.contains(&e.id))
                .map(|e| e.display_name().to_owned())
                .collect(),
            ..SwitchPlan::default()
        };
        for a in target.addons.iter().filter(|a| lock.get(&a.id).is_none()) {
            if stashed.contains(&a.id) {
                plan.restore.push(a.display_name());
            } else {
                plan.download.push(a.display_name());
            }
        }
        Ok(plan)
    }

    /// Make `name` the active profile: remember the current set in the active profile, stash
    /// managed addons the target doesn't list, restore the ones it does from the stash, and
    /// install the rest. Unmanaged folders are never touched.
    pub async fn switch_profile(&self, client: &Client, name: &str) -> Result<SwitchReport> {
        let (this, c, name) = (self.clone(), client.clone(), name.to_owned());
        let (mut report, missing) = self
            .blocking(move || {
                ensure_closed(&c)?;
                let path = this.paths().profiles(&c);
                let mut set = Profiles::load(&path)?;
                let lock_path = this.paths().lockfile(&c);
                let mut lock = this.lockfile(&c)?;
                set.sync_active(&lock.addons);
                let from = set.active.clone();
                let target = set
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| Error::NoProfile(name.clone()))?;
                set.active = Some(target.name.clone());
                set.save(&path)?;

                let (_, warning) =
                    this.snapshot_wtf(&c, &format!("before switching to {}", target.name));
                let mut report = SwitchReport {
                    profile: target.name.clone(),
                    warning,
                    from,
                    ..SwitchReport::default()
                };
                let root = install::stash_root(&c.addons)?;
                let leaving: Vec<LockEntry> = lock
                    .addons
                    .iter()
                    .filter(|e| !target.contains(&e.id))
                    .cloned()
                    .collect();
                for e in leaving {
                    match install::stash(&c.addons, &e, &root) {
                        Ok(()) => {
                            lock.remove(&e.id);
                            report.stashed.push(e.display_name().to_owned());
                        }
                        Err(err) => report
                            .failed
                            .push((e.display_name().to_owned(), err.to_string())),
                    }
                }
                let mut missing = Vec::new();
                let wanted: Vec<_> = target
                    .addons
                    .iter()
                    .filter(|a| lock.get(&a.id).is_none())
                    .collect();
                for a in wanted {
                    match install::unstash(&c.addons, &a.id, &root) {
                        Ok(Some(mut e)) => {
                            report.restored.push(e.display_name().to_owned());
                            e.installed_at = now_rfc3339();
                            lock.upsert(e);
                        }
                        Ok(None) => missing.push(a.clone()),
                        Err(err) => report.failed.push((a.display_name(), err.to_string())),
                    }
                }
                lock.save(&lock_path)?;
                Ok((report, missing))
            })
            .await?;

        let mut tasks = JoinSet::new();
        for a in missing {
            let (this, client) = (self.clone(), client.clone());
            tasks.spawn(async move {
                let opts = InstallOptions {
                    channel: a.channel,
                    version: a.version.clone(),
                    ..InstallOptions::default()
                };
                (a.display_name(), this.install(&client, &a.id, opts).await)
            });
        }
        while let Some(joined) = tasks.join_next().await {
            match joined.map_err(|e| Error::Task(e.to_string()))? {
                (_, Ok(done)) => report.installed.push(done.entry.display_name().to_owned()),
                (name, Err(e)) => report.failed.push((name, e.to_string())),
            }
        }
        let undo = report
            .from
            .clone()
            .filter(|f| !f.eq_ignore_ascii_case(&report.profile))
            .map(|profile| Undo::Switch { profile });
        self.log(
            client,
            &Event::now(
                Kind::Switch,
                format!("switched to profile {}", report.profile),
                undo,
            ),
        );
        Ok(report)
    }

    /// Load problems in the client's AddOns folder (see [`crate::health`]).
    pub fn health(&self, client: &Client) -> Result<Vec<Issue>> {
        let folders = if client.addons.is_dir() {
            local::scan(client)?
        } else {
            Vec::new()
        };
        Ok(health::check(
            &client.addons,
            &folders,
            &self.lockfile(client)?,
        ))
    }

    /// Folders parked by [`Engine::disable`].
    pub fn disabled(&self, client: &Client) -> Vec<String> {
        install::disabled(&client.addons)
    }

    /// Move an addon's folders (or one hand-installed folder) out of WoW's sight, keeping its
    /// lockfile entry. Returns the folders moved.
    pub async fn disable(&self, client: &Client, name: &str) -> Result<Vec<String>> {
        let (this, client, name) = (self.clone(), client.clone(), name.to_owned());
        self.blocking(move || {
            ensure_closed(&client)?;
            let present = client
                .addons
                .join(&name)
                .is_dir()
                .then(|| vec![name.clone()]);
            let folders = folders_named(&this.lockfile(&client)?, &name, present)?;
            install::disable(&client.addons, &folders)?;
            this.log(
                &client,
                &Event::now(
                    Kind::Disable,
                    format!("disabled {}", folders.join(", ")),
                    Some(Undo::Enable { name: name.clone() }),
                ),
            );
            Ok(folders)
        })
        .await
    }

    /// Undo [`Engine::disable`].
    pub async fn enable(&self, client: &Client, name: &str) -> Result<Vec<String>> {
        let (this, client, name) = (self.clone(), client.clone(), name.to_owned());
        self.blocking(move || {
            ensure_closed(&client)?;
            let parked = install::disabled(&client.addons)
                .into_iter()
                .find(|f| f.eq_ignore_ascii_case(&name))
                .map(|f| vec![f]);
            let folders = folders_named(&this.lockfile(&client)?, &name, parked)?;
            install::enable(&client.addons, &folders)?;
            this.log(
                &client,
                &Event::now(
                    Kind::Enable,
                    format!("enabled {}", folders.join(", ")),
                    Some(Undo::Disable { name: name.clone() }),
                ),
            );
            Ok(folders)
        })
        .await
    }

    /// Saved-settings snapshots, newest first.
    pub fn wtf_snapshots(&self, client: &Client) -> Vec<Snapshot> {
        wtf::list(&self.paths().wtf_backups(client))
    }

    /// Snapshot saved settings now; `None` when the client has none yet.
    pub async fn backup_wtf(&self, client: &Client) -> Result<Option<Snapshot>> {
        let (this, client) = (self.clone(), client.clone());
        self.blocking(move || {
            wtf::snapshot(
                &client.wtf(),
                &this.paths().wtf_backups(&client),
                "backed up by hand",
            )
        })
        .await
    }

    /// Put a snapshot's settings back (all of them, or just `only` addon folders', or just one
    /// `character`'s), after snapshotting the current ones. Returns how many files were written.
    pub async fn restore_wtf(
        &self,
        client: &Client,
        name: &str,
        only: Option<Vec<String>>,
        character: Option<String>,
    ) -> Result<usize> {
        let (this, client, name) = (self.clone(), client.clone(), name.to_owned());
        self.blocking(move || {
            ensure_closed(&client)?;
            let dir = this.paths().wtf_backups(&client);
            let snap = wtf::find(&dir, &name).ok_or_else(|| Error::NoBackup(name.clone()))?;
            let character = character
                .map(|q| {
                    characters::list(&client.wtf())
                        .into_iter()
                        .find(|c| c.is(&q))
                        .ok_or_else(|| Error::BadRef(format!("no character named {q}")))
                })
                .transpose()?;
            let under = character.as_ref().map(Character::rel_dir);
            let files = wtf::read(&snap.path, only.as_deref(), under.as_deref())?;
            let safety = wtf::snapshot(
                &client.wtf(),
                &dir,
                &format!("before restoring {}", crate::lockfile::rfc3339(snap.made)),
            )?;
            install::restore_files(&client.wtf(), &files)?;
            this.log(
                &client,
                &Event::now(
                    Kind::Restore,
                    format!(
                        "restored {} {}settings files from {}",
                        files.len(),
                        character
                            .map(|c| format!("{} ", c.name))
                            .unwrap_or_default(),
                        crate::lockfile::rfc3339(snap.made)
                    ),
                    safety.map(|s| Undo::RestoreWtf { snapshot: s.name }),
                ),
            );
            Ok(files.len())
        })
        .await
    }

    /// Everything kodo changed for the client, newest first.
    pub fn history(&self, client: &Client) -> Vec<Event> {
        history::list(&self.paths().history(client))
    }

    /// Carry out a history event's undo. Returns what was done, for display.
    pub async fn undo(&self, client: &Client, undo: &Undo) -> Result<String> {
        Ok(match undo {
            Undo::Rollback { addon } => {
                let r = self.rollback(client, &addon.to_string()).await?;
                format!(
                    "rolled back {} to {}",
                    r.entry.display_name(),
                    r.entry.version
                )
            }
            Undo::Remove { addon } => {
                let e = self.remove(client, &addon.to_string(), true).await?;
                format!("removed {}", e.display_name())
            }
            Undo::Enable { name } => {
                format!("enabled {}", self.enable(client, name).await?.join(", "))
            }
            Undo::Disable { name } => {
                format!("disabled {}", self.disable(client, name).await?.join(", "))
            }
            Undo::Switch { profile } => {
                let r = self.switch_profile(client, profile).await?;
                format!("switched to profile {}", r.profile)
            }
            Undo::RestoreWtf { snapshot } => {
                let n = self.restore_wtf(client, snapshot, None, None).await?;
                format!("restored {n} settings files")
            }
            Undo::CharacterAddons { character, states } => {
                let back = self
                    .set_character_addons(client, character, states.clone())
                    .await?;
                format!("set {} addon(s) back for {character}", back.len())
            }
        })
    }

    /// Library folders nothing installed uses (see [`health::unused_libraries`]).
    pub fn unused_libraries(&self, client: &Client) -> Result<Vec<String>> {
        if !client.addons.is_dir() {
            return Ok(Vec::new());
        }
        Ok(health::unused_libraries(
            &local::scan(client)?,
            &self.lockfile(client)?,
        ))
    }

    fn log(&self, client: &Client, event: &Event) {
        history::record(&self.paths().history(client), event);
    }

    /// The client's characters: addon switches, last played, what keeps settings per
    /// character, and the snapshots holding their settings.
    pub fn characters(&self, client: &Client) -> Vec<CharacterAddons> {
        let wtf = client.wtf();
        let snapshots = self.wtf_snapshots(client);
        characters::list(&wtf)
            .into_iter()
            .map(|c| {
                let dir = c.rel_dir();
                CharacterAddons {
                    states: characters::read(&wtf, &c),
                    last_played: characters::last_played(&wtf, &c),
                    saved: characters::saved(&wtf, &c),
                    snapshots: snapshots
                        .iter()
                        .map(|s| CharacterSnapshot {
                            name: s.name.clone(),
                            made: s.made,
                            reason: s.reason.clone(),
                            files: wtf::count_under(&s.path, &dir),
                        })
                        .filter(|s| s.files > 0)
                        .collect(),
                    character: c,
                }
            })
            .collect()
    }

    /// The client's region, as WoW last saw it.
    pub fn region(&self, client: &Client) -> Option<String> {
        characters::region(&client.wtf())
    }

    /// Give `to` the same addon switches as `from` for every folder in AddOns.
    pub async fn copy_character_addons(
        &self,
        client: &Client,
        from: &str,
        to: &str,
    ) -> Result<Vec<(String, bool)>> {
        let wtf = client.wtf();
        let src = characters::list(&wtf)
            .into_iter()
            .find(|c| c.is(from))
            .ok_or_else(|| Error::BadRef(format!("no character named {from}")))?;
        let states = characters::read(&wtf, &src);
        let changes = local::scan(client)?
            .into_iter()
            .map(|l| {
                let on = characters::is_enabled(&states, &l.folder);
                (l.folder, on)
            })
            .collect();
        self.set_character_addons(client, to, changes).await
    }

    /// Turn addon folders on or off for one character (by key or folder name). Returns the
    /// previous state of each folder that changed, to undo with.
    pub async fn set_character_addons(
        &self,
        client: &Client,
        character: &str,
        changes: Vec<(String, bool)>,
    ) -> Result<Vec<(String, bool)>> {
        let (this, client, character) = (self.clone(), client.clone(), character.to_owned());
        self.blocking(move || {
            ensure_closed(&client)?;
            let wtf = client.wtf();
            let c = characters::list(&wtf)
                .into_iter()
                .find(|c| c.is(&character))
                .ok_or_else(|| Error::BadRef(format!("no character named {character}")))?;
            let path = wtf.join(c.rel_file());
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let states = characters::parse(&existing);
            let changed: Vec<(String, bool)> = changes
                .into_iter()
                .filter(|(f, on)| characters::is_enabled(&states, f) != *on)
                .collect();
            if changed.is_empty() {
                return Ok(Vec::new());
            }
            let previous: Vec<(String, bool)> =
                changed.iter().map(|(f, on)| (f.clone(), !on)).collect();
            let text = characters::render(&existing, &changed);
            install::restore_files(&wtf, &[(c.rel_file(), text.into_bytes())])?;
            let on = changed.iter().filter(|(_, on)| *on).count();
            this.log(
                &client,
                &Event::now(
                    Kind::Characters,
                    format!(
                        "{}: turned on {on}, off {} addon(s)",
                        c.name,
                        changed.len() - on
                    ),
                    Some(Undo::CharacterAddons {
                        character: c.key(),
                        states: previous.clone(),
                    }),
                ),
            );
            Ok(previous)
        })
        .await
    }

    /// Load just a profile's addons on one character: its installed folders on, every other
    /// folder kodo manages off. Hand-installed folders are left as they are.
    pub async fn apply_profile_to_character(
        &self,
        client: &Client,
        character: &str,
        profile: &str,
    ) -> Result<Vec<(String, bool)>> {
        let set = self.profiles(client)?;
        let p = set
            .get(profile)
            .ok_or_else(|| Error::NoProfile(profile.to_owned()))?;
        let lock = self.lockfile(client)?;
        let changes = lock
            .addons
            .iter()
            .flat_map(|e| {
                let on = p.contains(&e.id);
                e.folders.iter().map(move |f| (f.clone(), on))
            })
            .collect();
        self.set_character_addons(client, character, changes).await
    }

    /// Install a zip from disk. Its folders stay hand-installed (Link them to a source to
    /// update them); hand-installed folders it replaces are backed up first.
    pub async fn install_zip(&self, client: &Client, zip: &Path) -> Result<ZipInstalled> {
        let (this, client, zip) = (self.clone(), client.clone(), zip.to_path_buf());
        self.blocking(move || {
            ensure_closed(&client)?;
            let staged = install::extract(&zip, None, &client.addons)?;
            let lock = this.lockfile(&client)?;
            let disabled = install::disabled(&client.addons);
            for f in &staged.folders {
                if let Some(owner) = lock.owner_of(f) {
                    return Err(Error::Conflict {
                        folder: f.clone(),
                        owner: owner.id.to_string(),
                    });
                }
                if disabled.iter().any(|d| d.eq_ignore_ascii_case(f)) {
                    return Err(Error::Disabled(f.clone()));
                }
            }
            let replaced: Vec<String> = staged
                .folders
                .iter()
                .filter(|f| client.addons.join(f).exists())
                .cloned()
                .collect();
            let made = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let backup = this
                .paths()
                .backups(&client)
                .join("from-zip")
                .join(made.to_string());
            install::swap(
                &client.addons,
                staged.path(),
                &staged.folders,
                &replaced,
                &backup,
            )?;
            let file = zip.file_name().map(|n| n.to_string_lossy().into_owned());
            this.log(
                &client,
                &Event::now(
                    Kind::Install,
                    format!(
                        "installed {} from {}",
                        staged.folders.join(", "),
                        file.unwrap_or_default()
                    ),
                    None,
                ),
            );
            Ok(ZipInstalled {
                folders: staged.folders.clone(),
                replaced,
            })
        })
        .await
    }

    /// Match an imported list against the sources: its own refs as given, names by searching.
    pub async fn resolve_import(&self, client: &Client, wanted: Vec<Wanted>) -> Vec<ImportMatch> {
        let lock = self.lockfile(client).ok();
        let installed = |id: &AddonRef| lock.as_ref().is_some_and(|l| l.get(id).is_some());
        let mut tasks = JoinSet::new();
        for (i, w) in wanted.into_iter().enumerate() {
            let (this, flavor) = (self.clone(), client.flavor.clone());
            tasks.spawn(async move {
                if w.addon.is_some() {
                    return (i, w, None);
                }
                let q = Query {
                    text: w.name.clone(),
                    limit: 5,
                    ..Query::default()
                };
                let hit = this.search(&q, &flavor, None).await.hits.into_iter().next();
                (i, w, hit)
            });
        }
        let mut out = Vec::new();
        while let Some(Ok((i, w, hit))) = tasks.join_next().await {
            let m = match (&w.addon, hit) {
                (Some(id), _) => ImportMatch {
                    addon: Some(id.clone()),
                    name: w.name.clone(),
                    exact: true,
                    installed: installed(id),
                    wanted: w,
                },
                (None, Some(h)) => ImportMatch {
                    exact: same_name(&h.name, &w.name),
                    installed: installed(&h.addon),
                    addon: Some(h.addon),
                    name: h.name,
                    wanted: w,
                },
                (None, None) => ImportMatch {
                    addon: None,
                    name: w.name.clone(),
                    exact: false,
                    installed: false,
                    wanted: w,
                },
            };
            out.push((i, m));
        }
        out.sort_by_key(|(i, _)| *i);
        out.into_iter().map(|(_, m)| m).collect()
    }

    /// When each addon folder was last loaded (lowercased folder → Unix seconds); see
    /// [`wtf::last_saved`].
    pub fn last_loaded(&self, client: &Client) -> BTreeMap<String, u64> {
        wtf::last_saved(&client.wtf())
    }

    /// Latest versions already shown to the user, for "new since last time".
    pub fn seen(&self, client: &Client) -> BTreeMap<AddonRef, String> {
        let text = std::fs::read_to_string(self.paths().seen(client)).unwrap_or_default();
        toml::from_str::<BTreeMap<String, String>>(&text)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| Some((k.parse().ok()?, v)))
            .collect()
    }

    /// Remember these latest versions as shown.
    pub fn mark_seen(&self, client: &Client, shown: &[(AddonRef, String)]) -> Result<()> {
        let mut seen: BTreeMap<String, String> = self
            .seen(client)
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        for (id, v) in shown {
            seen.insert(id.to_string(), v.clone());
        }
        let path = self.paths().seen(client);
        let text = toml::to_string(&seen).map_err(|e| Error::BadFile {
            path: path.clone(),
            message: e.to_string(),
        })?;
        crate::config::write_atomic(&path, text.as_bytes())
    }

    /// Snapshot saved settings before a change. A failure is a warning, never a blocker.
    fn snapshot_wtf(&self, client: &Client, reason: &str) -> (Option<String>, Option<String>) {
        match wtf::snapshot(&client.wtf(), &self.paths().wtf_backups(client), reason) {
            Ok(s) => (s.map(|s| s.name), None),
            Err(e) => (
                None,
                Some(format!("saved settings were not backed up: {e}")),
            ),
        }
    }

    /// Run filesystem work off the async runtime, serialized per engine.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let _guard = self.inner.disk.lock().await;
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| Error::Task(e.to_string()))?
    }
}

/// Refuse to touch a client's files while its game is running.
fn ensure_closed(client: &Client) -> Result<()> {
    if client.game_running() {
        Err(Error::GameRunning(client.name.clone()))
    } else {
        Ok(())
    }
}

/// A managed addon's folders by name, else `fallback` (a plain folder), else not installed.
fn folders_named(
    lock: &Lockfile,
    name: &str,
    fallback: Option<Vec<String>>,
) -> Result<Vec<String>> {
    match lock.find(name) {
        Ok(e) => Ok(e.folders.clone()),
        Err(Error::NotInstalled(_)) => fallback.ok_or_else(|| Error::NotInstalled(name.to_owned())),
        Err(e) => Err(e),
    }
}

/// Source refs a TOC declares, in source priority order.
fn header_refs(toc: &crate::toc::Toc) -> Vec<AddonRef> {
    let mut refs = Vec::new();
    let get = |k: &str| {
        toc.extra
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.trim())
    };
    let github = toc
        .extra
        .values()
        .filter(|v| v.to_ascii_lowercase().contains("github.com/"))
        .find_map(|v| v.parse::<AddonRef>().ok());
    refs.extend(github);
    for (key, source) in [
        ("X-WoWI-ID", SourceId::Wowi),
        ("X-Curse-Project-ID", SourceId::Curseforge),
        ("X-Wago-ID", SourceId::Wago),
    ] {
        if let Some(r) = get(key).and_then(|v| format!("{source}:{v}").parse().ok()) {
            refs.push(r);
        }
    }
    refs
}

/// Whether a backup dir path is a kodo backup (used by cache size reporting).
pub fn is_backup_dir(dir: &Path) -> bool {
    dir.join(BACKUP_ENTRY).is_file()
}

/// Names equal once case, spaces and punctuation are ignored ("Details!" = "details").
fn same_name(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    norm(a) == norm(b)
}

/// Folder name up to its first `-` or `_`: `DBM-Core` and `DBM-PvP` share the stem `DBM`.
fn stem(folder: &str) -> String {
    folder
        .split(['-', '_'])
        .next()
        .unwrap_or(folder)
        .to_ascii_lowercase()
}
