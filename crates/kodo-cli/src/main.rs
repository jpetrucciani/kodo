use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use kodo_core::addon::{AddonRef, Channel, Query, Sort, SourceId};
use kodo_core::config::{Config, Paths};
use kodo_core::detect::{self, Client};
use kodo_core::engine::{Engine, InstallOptions, Installed, UpdateCheck};
use kodo_core::flavor::Flavor;
use kodo_core::health::Issue;
use kodo_core::lockfile::LockEntry;
use kodo_core::profile::Profile;
use kodo_core::source::Progress;
use serde::Serialize;

const EXIT_UPDATES: u8 = 1;
const EXIT_USER: u8 = 2;
const EXIT_NETWORK: u8 = 3;

#[derive(Parser)]
#[command(name = "kodo", version, about = "A small, fast WoW addon manager")]
struct Cli {
    /// Print JSON instead of human-readable output
    #[arg(long, global = true)]
    json: bool,
    /// Extra WoW install, client or Interface/AddOns folder to include (repeatable)
    #[arg(long, global = true)]
    path: Vec<PathBuf>,
    /// Client to act on, by flavor id or name; defaults to Forever, then the only client
    #[arg(long, global = true)]
    client: Option<String>,
    /// Keep config, lockfiles and caches in kodo-data/ next to the binary
    #[arg(long, global = true)]
    portable: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List detected and configured clients
    Clients,
    /// Match installed addons to sources and record them in the lockfile
    Scan {
        /// Report matches without writing the lockfile
        #[arg(long)]
        dry_run: bool,
    },
    /// List addons kodo manages in the client
    List,
    /// Search enabled sources; with no text, browse the GitHub catalogue for the client's flavor
    Search {
        query: Vec<String>,
        #[arg(long)]
        source: Option<SourceId>,
        /// relevance, downloads, updated or name
        #[arg(long, default_value = "relevance")]
        sort: Sort,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// Install an addon: a ref (github:owner/repo, wowi:123, cf:456), a project link, or a
    /// .zip on disk. With --from, install a list (one per line, or a WowUp export string)
    Install {
        target: Option<String>,
        /// File holding an addon list, or - for stdin
        #[arg(long, conflicts_with = "target")]
        from: Option<std::path::PathBuf>,
        /// With --from: install the matches instead of just listing them
        #[arg(long, requires = "from")]
        yes: bool,
        #[arg(long, default_value = "stable")]
        channel: Channel,
        /// Install and pin this exact version
        #[arg(long)]
        version: Option<String>,
        /// Replace existing folders kodo doesn't manage
        #[arg(long)]
        force: bool,
    },
    /// Update the named addons, or every addon with --all
    Update {
        names: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// List addons with updates; exits 1 if there are any
    Outdated,
    /// Stop updating an addon, optionally installing a specific version first
    Pin {
        name: String,
        #[arg(long)]
        version: Option<String>,
    },
    /// Let a pinned addon update again
    Unpin { name: String },
    /// Restore the previous version, or undo a remove
    Rollback { name: String },
    /// Remove addons (kept in backups; undo with rollback). Dependents go first, so a library
    /// can go along with the addons using it
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
        /// Remove even if addons outside this list require them
        #[arg(long)]
        force: bool,
    },
    /// Show release notes from the installed version up to the latest
    Changes { name: String },
    /// Find addons WoW won't load: out-of-date TOCs, misplaced folders, duplicates, missing dependencies
    Health,
    /// Move an addon (or a hand-installed folder) out of WoW's sight without removing it
    Disable { name: String },
    /// Bring back a disabled addon or folder
    Enable { name: String },
    /// Backups of saved addon settings (WTF SavedVariables)
    #[command(subcommand)]
    Wtf(WtfCmd),
    /// What kodo changed, newest first
    History {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Undo the newest change that can be undone
    Undo,
    /// Which addons each character loads (WTF/.../AddOns.txt)
    #[command(subcommand, alias = "chars")]
    Character(CharacterCmd),
    /// Print kodo's version and whether a newer release is out
    Version,
    /// Named sets of addons you can switch between and share as codes
    #[command(subcommand)]
    Profile(ProfileCmd),
}

#[derive(Subcommand)]
enum CharacterCmd {
    /// List characters
    List,
    /// Show which installed addon folders a character loads
    Show { name: String },
    /// Turn addon folders on or off for a character
    Set {
        name: String,
        #[arg(long, value_delimiter = ',')]
        on: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        off: Vec<String>,
    },
    /// Load only a profile's addons on a character
    Apply { name: String, profile: String },
    /// Give a character the same addon switches as another
    Copy { from: String, to: String },
}

#[derive(Subcommand)]
enum WtfCmd {
    /// List snapshots, newest first
    List,
    /// Snapshot saved settings now
    Backup,
    /// Put a snapshot's settings back (current ones are snapshotted first)
    Restore {
        /// Snapshot name from `kodo wtf list`
        name: String,
        /// Comma-separated addon folders to restore instead of everything
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Restore only this character's settings
        #[arg(long)]
        character: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// List profiles; * marks the active one
    List,
    /// Save installed addons (or just --only ones) as a profile
    Save {
        name: String,
        /// Comma-separated addon names or refs to include
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
    },
    /// Stash addons the profile doesn't list, restore or install the ones it does
    Switch {
        name: String,
        /// Show what would change without changing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Print a shareable code for a saved profile, or for installed addons (with --only)
    Code {
        name: String,
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
    },
    /// Save a profile from someone's code
    Import {
        code: String,
        /// Switch to it right away
        #[arg(long)]
        switch: bool,
    },
    /// Forget a profile (stashed addons stay stashed)
    Delete { name: String },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            let network = e.chain().any(|c| {
                c.downcast_ref::<kodo_core::Error>()
                    .is_some_and(kodo_core::Error::is_network)
                    || c.downcast_ref::<kodo_core::SourceError>().is_some()
            });
            ExitCode::from(if network { EXIT_NETWORK } else { EXIT_USER })
        }
    }
}

struct Ctx {
    json: bool,
    engine: Engine,
    clients: Vec<Client>,
    client_arg: Option<String>,
}

impl Ctx {
    fn client(&self) -> anyhow::Result<&Client> {
        pick(&self.clients, self.client_arg.as_deref())
    }

    fn print<T: Serialize>(&self, value: &T, human: impl FnOnce(&T)) -> anyhow::Result<()> {
        if self.json {
            println!("{}", serde_json::to_string_pretty(value)?);
        } else {
            human(value);
        }
        Ok(())
    }
}

async fn run(cli: Cli) -> anyhow::Result<u8> {
    let paths = Paths::resolve(cli.portable)?;
    let config = Config::load(&paths.config_file)?;
    let sources = kodo_sources::build(&kodo_sources::SourcesOptions::from_config(&paths, &config))?;
    let (mut clients, errors) = config.clients();
    for e in &errors {
        eprintln!("warning: {e}");
    }
    for p in &cli.path {
        for c in detect::from_path(p)? {
            if !clients.iter().any(|k| k.addons == c.addons) {
                clients.push(c);
            }
        }
    }
    if let Command::Version = cli.command {
        println!("kodo {}", env!("CARGO_PKG_VERSION"));
        match kodo_sources::check_kodo(&paths.source_cache(), env!("CARGO_PKG_VERSION")).await? {
            Some(r) => println!("kodo {} is out: {}", r.version, r.url),
            None => println!("up to date"),
        }
        return Ok(0);
    }
    let ctx = Ctx {
        json: cli.json,
        engine: Engine::new(paths, sources, config.concurrency),
        clients,
        client_arg: cli.client,
    };

    match cli.command {
        Command::Clients => ctx.print(&ctx.clients, |clients| {
            if clients.is_empty() {
                println!("no clients found; pass --path <WoW folder>");
            }
            for c in clients {
                let version = c.version.as_deref().unwrap_or("unknown version");
                println!("{:<12} {:<16} {}", c.name, version, c.addons.display());
            }
        })?,
        Command::Scan { dry_run } => {
            let client = ctx.client()?;
            let report = ctx.engine.scan(client, !dry_run).await?;
            ctx.print(&report, |r| {
                for e in &r.adopted {
                    println!(
                        "adopted  {:<32} {:<28} {}",
                        e.display_name(),
                        e.id,
                        e.version
                    );
                    if e.folders.len() > 1 {
                        println!("         folders: {}", e.folders.join(", "));
                    }
                }
                for e in &r.managed {
                    println!(
                        "managed  {:<32} {:<28} {}",
                        e.display_name(),
                        e.id,
                        e.version
                    );
                }
                for e in &r.missing {
                    println!(
                        "missing  {:<32} {:<28} folders gone from disk",
                        e.display_name(),
                        e.id
                    );
                }
                for l in &r.local {
                    let title = l
                        .toc
                        .as_ref()
                        .and_then(|t| t.title.as_deref())
                        .unwrap_or("-");
                    match r.suggestions.iter().find(|m| m.folders.contains(&l.folder)) {
                        Some(m) => println!(
                            "local    {:<32} {title}  looks like {} (link: kodo install {} --force)",
                            l.folder, m.addon, m.addon
                        ),
                        None => println!("local    {:<32} {title}", l.folder),
                    }
                }
                for (src, e) in &r.errors {
                    eprintln!("warning: {}: {e}", src.label());
                }
                if dry_run && !r.adopted.is_empty() {
                    println!("(dry run: lockfile not written)");
                }
            })?;
        }
        Command::List => {
            let lock = ctx.engine.lockfile(ctx.client()?)?;
            ctx.print(&lock.addons, |entries| {
                for e in entries {
                    let pin = if e.pinned { "pinned" } else { "" };
                    println!(
                        "{:<32} {:<16} {:<28} {pin}",
                        e.display_name(),
                        e.version,
                        e.id
                    );
                }
            })?;
        }
        Command::Search {
            query,
            source,
            sort,
            limit,
        } => {
            let flavor = ctx.client().map_or(Flavor::Forever, |c| c.flavor.clone());
            let q = Query {
                text: query.join(" "),
                limit,
                sort,
            };
            let results = ctx.engine.search(&q, &flavor, source).await;
            for (src, e) in &results.errors {
                eprintln!("warning: {}: {e}", src.label());
            }
            if results.hits.is_empty() && !results.errors.is_empty() {
                // Every source that answered failed; the reasons were printed above.
                return Ok(EXIT_NETWORK);
            }
            ctx.print(&results.hits, |hits| {
                for h in hits {
                    let downloads = h.downloads.map(|d| d.to_string()).unwrap_or_default();
                    println!(
                        "{:<40} {:<32} {downloads:>10}  {:<10}  {}",
                        h.addon.to_string(),
                        h.name,
                        h.updated.as_deref().unwrap_or(""),
                        h.summary.as_deref().unwrap_or("")
                    );
                }
            })?;
        }
        Command::Install {
            from: Some(file),
            yes,
            channel,
            ..
        } => {
            let text = if file.as_os_str() == "-" {
                std::io::read_to_string(std::io::stdin())?
            } else {
                std::fs::read_to_string(&file)?
            };
            let parsed = kodo_core::import::parse(&text)?;
            let client = ctx.client()?;
            let matches = ctx.engine.resolve_import(client, parsed.wanted).await;
            let todo: Vec<AddonRef> = matches
                .iter()
                .filter(|m| m.exact && !m.installed)
                .filter_map(|m| m.addon.clone())
                .collect();
            ctx.print(&matches, |ms| {
                for m in ms {
                    let what = match (&m.addon, m.exact, m.installed) {
                        (Some(_), _, true) => "installed already",
                        (Some(_), true, false) => "will install",
                        (Some(_), false, false) => "near miss, skipped",
                        (None, ..) => "not found",
                    };
                    let id = m
                        .addon
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    println!("{:<28} {:<36} {what}", m.wanted.name, id);
                }
            })?;
            if !yes {
                if !ctx.json {
                    println!("{} to install; run again with --yes", todo.len());
                }
                return Ok(0);
            }
            let bars = Bars::new(ctx.json);
            let mut failed = 0;
            for addon in todo {
                let opts = InstallOptions {
                    channel,
                    progress: bars.add(&addon.to_string()),
                    ..InstallOptions::default()
                };
                match ctx.engine.install(client, &addon, opts).await {
                    Ok(i) => print_installed(&i),
                    Err(e) => {
                        failed += 1;
                        eprintln!("{addon}: {e}");
                    }
                }
            }
            bars.clear();
            if failed > 0 {
                bail!("{failed} install(s) failed");
            }
        }
        Command::Install {
            target: Some(target),
            channel,
            version,
            force,
            ..
        } => {
            let client = ctx.client()?;
            let as_path = std::path::Path::new(&target);
            if as_path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                && as_path.is_file()
            {
                let done = ctx.engine.install_zip(client, as_path).await?;
                ctx.print(&done, |d| {
                    println!(
                        "installed {} (hand-installed; `kodo scan` can link it to a source)",
                        d.folders.join(", ")
                    );
                    if !d.replaced.is_empty() {
                        println!(
                            "replaced {} (old copy kept in kodo's backups)",
                            d.replaced.join(", ")
                        );
                    }
                })?;
                return Ok(0);
            }
            let addon: AddonRef = target.parse()?;
            let bars = Bars::new(ctx.json);
            let opts = InstallOptions {
                channel,
                version,
                force,
                progress: bars.add(&addon.to_string()),
            };
            let installed = ctx.engine.install(client, &addon, opts).await?;
            bars.clear();
            ctx.print(&installed, print_installed)?;
        }
        Command::Install { .. } => {
            bail!(kodo_core::Error::BadRef(
                "name an addon, a link or a .zip, or pass --from FILE".into()
            ));
        }
        Command::History { limit } => {
            let events: Vec<_> = ctx
                .engine
                .history(ctx.client()?)
                .into_iter()
                .take(limit)
                .collect();
            ctx.print(&events, |events| {
                if events.is_empty() {
                    println!("nothing yet");
                }
                for e in events {
                    let mark = if e.undo.is_some() { "" } else { "  (no undo)" };
                    println!("{}  {}{mark}", kodo_core::lockfile::rfc3339(e.at), e.what);
                }
            })?;
        }
        Command::Undo => {
            let client = ctx.client()?;
            let Some((event, undo)) = ctx
                .engine
                .history(client)
                .into_iter()
                .find_map(|e| e.undo.clone().map(|u| (e, u)))
            else {
                bail!(kodo_core::Error::BadRef("nothing to undo".into()));
            };
            let done = ctx.engine.undo(client, &undo).await?;
            println!("undid \"{}\": {done}", event.what);
        }
        Command::Character(cmd) => {
            let client = ctx.client()?;
            match cmd {
                CharacterCmd::List => {
                    let chars = ctx.engine.characters(client);
                    ctx.print(&chars, |chars| {
                        if chars.is_empty() {
                            println!("no characters yet (log in once)");
                        }
                        for c in chars {
                            let off = c.states.values().filter(|on| !**on).count();
                            let played = c.last_played.map_or_else(
                                || "never logged out".to_owned(),
                                |s| {
                                    let at = kodo_core::lockfile::rfc3339(s);
                                    format!("played {}", at.split('T').next().unwrap_or(&at))
                                },
                            );
                            println!("{:<32} {off} addon(s) off, {played}", c.character.key());
                        }
                    })?;
                }
                CharacterCmd::Show { name } => {
                    let chars = ctx.engine.characters(client);
                    let Some(c) = chars.into_iter().find(|c| c.character.is(&name)) else {
                        bail!(kodo_core::Error::BadRef(format!(
                            "no character named {name}"
                        )));
                    };
                    let folders: Vec<(String, bool)> = kodo_core::local::scan(client)?
                        .into_iter()
                        .map(|l| {
                            let on = kodo_core::characters::is_enabled(&c.states, &l.folder);
                            (l.folder, on)
                        })
                        .collect();
                    ctx.print(&folders, |folders| {
                        for (f, on) in folders {
                            println!("{}  {f}", if *on { "on " } else { "off" });
                        }
                    })?;
                }
                CharacterCmd::Set { name, on, off } => {
                    let changes = on
                        .into_iter()
                        .map(|f| (f, true))
                        .chain(off.into_iter().map(|f| (f, false)))
                        .collect();
                    let prev = ctx
                        .engine
                        .set_character_addons(client, &name, changes)
                        .await?;
                    println!("changed {} folder(s) for {name}", prev.len());
                }
                CharacterCmd::Copy { from, to } => {
                    let prev = ctx.engine.copy_character_addons(client, &from, &to).await?;
                    println!("{to} now matches {from}; changed {} folder(s)", prev.len());
                }
                CharacterCmd::Apply { name, profile } => {
                    let prev = ctx
                        .engine
                        .apply_profile_to_character(client, &name, &profile)
                        .await?;
                    println!(
                        "{name} now loads {profile}; changed {} folder(s)",
                        prev.len()
                    );
                }
            }
        }
        Command::Update {
            names,
            all,
            dry_run,
        } => {
            if names.is_empty() && !all {
                bail!(kodo_core::Error::BadRef(
                    "name addons to update, or pass --all".into()
                ));
            }
            let client = ctx.client()?;
            let lock = ctx.engine.lockfile(client)?;
            let wanted: Vec<_> = names
                .iter()
                .map(|n| lock.find(n).map(|e| e.id.clone()))
                .collect::<Result<_, _>>()?;
            let checks: Vec<UpdateCheck> = ctx
                .engine
                .check_updates(client)
                .await?
                .into_iter()
                .filter(|c| all || wanted.contains(&c.entry.id))
                .collect();
            report_check_errors(&checks);
            let pending: Vec<&UpdateCheck> = checks.iter().filter(|c| c.has_update()).collect();
            if dry_run || pending.is_empty() {
                let rows: Vec<_> = pending.iter().map(|c| Row::from(*c)).collect();
                return ctx
                    .print(&rows, |rows| {
                        if rows.is_empty() {
                            println!("everything is up to date");
                        }
                        rows.iter().for_each(Row::print);
                    })
                    .map(|()| 0);
            }
            let bars = Bars::new(ctx.json);
            let mut jobs = tokio::task::JoinSet::new();
            for check in pending {
                let (engine, client, check) = (ctx.engine.clone(), client.clone(), check.clone());
                let progress = bars.add(check.entry.display_name());
                jobs.spawn(async move {
                    let name = check.entry.display_name().to_owned();
                    (name, engine.apply_update(&client, &check, progress).await)
                });
            }
            let mut done = Vec::new();
            let mut failed = 0;
            while let Some(joined) = jobs.join_next().await {
                let (name, res) = joined.context("update task panicked")?;
                match res {
                    Ok(i) => done.push(i),
                    Err(e) => {
                        failed += 1;
                        bars.println(&format!("error: {name}: {e}"));
                    }
                }
            }
            bars.clear();
            ctx.print(&done, |done| done.iter().for_each(print_installed))?;
            if failed > 0 {
                bail!("{failed} update(s) failed");
            }
        }
        Command::Outdated => {
            let checks = ctx.engine.check_updates(ctx.client()?).await?;
            report_check_errors(&checks);
            let rows: Vec<Row> = checks
                .iter()
                .filter(|c| c.has_update())
                .map(Row::from)
                .collect();
            let any = !rows.is_empty();
            ctx.print(&rows, |rows| rows.iter().for_each(Row::print))?;
            return Ok(if any { EXIT_UPDATES } else { 0 });
        }
        Command::Pin { name, version } => {
            let e = ctx
                .engine
                .pin(ctx.client()?, &name, version.as_deref())
                .await?;
            ctx.print(&e, |e| {
                println!("pinned {} at {}", e.display_name(), e.version)
            })?;
        }
        Command::Unpin { name } => {
            let e = ctx.engine.unpin(ctx.client()?, &name).await?;
            ctx.print(&e, |e| println!("unpinned {}", e.display_name()))?;
        }
        Command::Rollback { name } => {
            let r = ctx.engine.rollback(ctx.client()?, &name).await?;
            ctx.print(&r, |r| {
                println!("restored {} {}", r.entry.display_name(), r.entry.version);
                if r.settings > 0 {
                    println!(
                        "restored {} saved-settings file(s) from before the update",
                        r.settings
                    );
                }
                if let Some(w) = &r.warning {
                    eprintln!("warning: {w}");
                }
            })?;
        }
        Command::Remove { names, force } => {
            let results = ctx.engine.remove_many(ctx.client()?, &names, force).await?;
            let rows: Vec<Removed> = results
                .into_iter()
                .map(|(name, res)| match res {
                    Ok(entry) => Removed {
                        name,
                        removed: Some(entry),
                        error: None,
                    },
                    Err(e) => Removed {
                        name,
                        removed: None,
                        error: Some(e.to_string()),
                    },
                })
                .collect();
            ctx.print(&rows, |rows| {
                for r in rows {
                    match (&r.removed, &r.error) {
                        (Some(e), _) => println!(
                            "removed {} (undo with `kodo rollback {}`)",
                            e.display_name(),
                            e.id
                        ),
                        (None, Some(err)) => eprintln!("kept {}: {err}", r.name),
                        (None, None) => {}
                    }
                }
            })?;
            let failed = rows.iter().filter(|r| r.error.is_some()).count();
            if failed > 0 {
                bail!("{failed} addon(s) not removed");
            }
        }
        Command::Changes { name } => {
            let client = ctx.client()?;
            let entry = ctx.engine.lockfile(client)?.find(&name)?.clone();
            let latest = ctx
                .engine
                .resolve(client, &entry.id, entry.channel, None)
                .await?;
            let check = UpdateCheck {
                entry,
                latest: Some(latest),
                error: None,
            };
            let changes = if check.has_update() {
                ctx.engine.changes(&check).await?
            } else {
                Vec::new()
            };
            ctx.print(&changes, |changes| {
                if changes.is_empty() {
                    println!(
                        "{} {} is the latest",
                        check.entry.display_name(),
                        check.entry.version
                    );
                }
                for c in changes {
                    let date = c.published_at.as_deref().unwrap_or_default();
                    println!("## {} {}", c.version, date.get(..10).unwrap_or(date));
                    println!("{}\n", c.notes.as_deref().unwrap_or("(no notes)"));
                }
            })?;
        }
        Command::Health => {
            let client = ctx.client()?;
            let issues = ctx.engine.health(client)?;
            let disabled = ctx.engine.disabled(client);
            let unused = ctx.engine.unused_libraries(client)?;
            ctx.print(&issues, |issues| {
                if issues.is_empty() {
                    println!("no problems found");
                }
                for i in issues {
                    println!("{}", describe(i));
                }
                if !disabled.is_empty() {
                    println!("disabled: {}", disabled.join(", "));
                }
                if !unused.is_empty() {
                    println!("probably unused libraries: {}", unused.join(", "));
                }
            })?;
            return Ok(if issues.is_empty() { 0 } else { EXIT_UPDATES });
        }
        Command::Disable { name } => {
            let folders = ctx.engine.disable(ctx.client()?, &name).await?;
            ctx.print(&folders, |f| {
                println!("disabled {} (undo with `kodo enable {name}`)", f.join(", "));
            })?;
        }
        Command::Enable { name } => {
            let folders = ctx.engine.enable(ctx.client()?, &name).await?;
            ctx.print(&folders, |f| println!("enabled {}", f.join(", ")))?;
        }
        Command::Wtf(cmd) => {
            let client = ctx.client()?;
            match cmd {
                WtfCmd::List => {
                    let snaps = ctx.engine.wtf_snapshots(client);
                    ctx.print(&snaps, |snaps| {
                        if snaps.is_empty() {
                            println!("no snapshots yet");
                        }
                        for s in snaps {
                            println!(
                                "{:<32} {}  {:>4} files  {}",
                                s.name,
                                kodo_core::lockfile::rfc3339(s.made),
                                s.files,
                                s.reason
                            );
                        }
                    })?;
                }
                WtfCmd::Backup => {
                    let snap = ctx.engine.backup_wtf(client).await?;
                    ctx.print(&snap, |s| match s {
                        Some(s) => println!("saved {} ({} files)", s.name, s.files),
                        None => println!("no saved settings to back up yet"),
                    })?;
                }
                WtfCmd::Restore {
                    name,
                    only,
                    character,
                } => {
                    let only = (!only.is_empty()).then_some(only);
                    let n = ctx
                        .engine
                        .restore_wtf(client, &name, only, character)
                        .await?;
                    ctx.print(&n, |n| println!("restored {n} file(s)"))?;
                }
            }
        }
        Command::Version => {}
        Command::Profile(cmd) => return profile(&ctx, cmd).await,
    }
    Ok(0)
}

async fn profile(ctx: &Ctx, cmd: ProfileCmd) -> anyhow::Result<u8> {
    let client = ctx.client()?;
    // --only names → refs, via the lockfile so names and refs both work.
    let only = |names: &[String]| -> anyhow::Result<Option<Vec<AddonRef>>> {
        if names.is_empty() {
            return Ok(None);
        }
        let lock = ctx.engine.lockfile(client)?;
        names
            .iter()
            .map(|n| Ok(lock.find(n.trim())?.id.clone()))
            .collect::<anyhow::Result<_>>()
            .map(Some)
    };
    match cmd {
        ProfileCmd::List => {
            let set = ctx.engine.profiles(client)?;
            ctx.print(&set, |set| {
                for p in &set.profiles {
                    let active = set
                        .active
                        .as_deref()
                        .is_some_and(|a| a.eq_ignore_ascii_case(&p.name));
                    let names: Vec<String> = p.addons.iter().map(|a| a.display_name()).collect();
                    println!(
                        "{} {:<24} {}",
                        if active { "*" } else { " " },
                        p.name,
                        names.join(", ")
                    );
                }
            })?;
        }
        ProfileCmd::Save { name, only: names } => {
            let p = ctx
                .engine
                .save_profile(client, &name, only(&names)?)
                .await?;
            ctx.print(&p, |p| {
                println!("saved    {} ({} addons)", p.name, p.addons.len());
            })?;
        }
        ProfileCmd::Switch {
            name,
            dry_run: true,
        } => {
            let plan = ctx.engine.plan_switch(client, &name)?;
            ctx.print(&plan, |p| {
                for (what, names) in [
                    ("stash", &p.stash),
                    ("restore", &p.restore),
                    ("download", &p.download),
                ] {
                    for n in names {
                        println!("{what:<9}{n}");
                    }
                }
                println!("(dry run: switching to {} changes nothing yet)", p.profile);
            })?;
        }
        ProfileCmd::Switch { name, .. } => return switch(ctx, client, &name).await,
        ProfileCmd::Code { name, only: names } => {
            let profile = match only(&names)? {
                Some(ids) => {
                    Profile::from_entries(&name, &ctx.engine.lockfile(client)?.addons, Some(&ids))
                }
                None => ctx
                    .engine
                    .profiles(client)?
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| kodo_core::Error::NoProfile(name.clone()))?,
            };
            println!("{}", profile.to_code(&client.flavor));
        }
        ProfileCmd::Import { code, switch: now } => {
            let (profile, flavor) = Profile::from_code(&code)?;
            if flavor != client.flavor {
                eprintln!(
                    "warning: this code was made for {}; some addons may have no {} build",
                    flavor.label(),
                    client.flavor.label()
                );
            }
            let p = ctx.engine.add_profile(client, profile).await?;
            ctx.print(&p, |p| {
                println!("imported {} ({} addons)", p.name, p.addons.len());
            })?;
            if now {
                return switch(ctx, client, &p.name).await;
            }
        }
        ProfileCmd::Delete { name } => {
            let p = ctx.engine.delete_profile(client, &name).await?;
            ctx.print(&p, |p| println!("deleted  {}", p.name))?;
        }
    }
    Ok(0)
}

async fn switch(ctx: &Ctx, client: &Client, name: &str) -> anyhow::Result<u8> {
    let r = ctx.engine.switch_profile(client, name).await?;
    ctx.print(&r, |r| {
        for n in &r.stashed {
            println!("stashed  {n}");
        }
        for n in &r.restored {
            println!("restored {n}");
        }
        for n in &r.installed {
            println!("installed {n}");
        }
        println!("now on   {}", r.profile);
        if let Some(w) = &r.warning {
            eprintln!("warning: {w}");
        }
    })?;
    for (n, e) in &r.failed {
        eprintln!("error: {n}: {e}");
    }
    if r.failed.is_empty() {
        Ok(0)
    } else {
        bail!("{} addon(s) didn't switch over", r.failed.len())
    }
}

fn describe(i: &Issue) -> String {
    match i {
        Issue::OutOfDate { folder } => format!("out of date  {folder}: TOC is older than the game"),
        Issue::NoToc {
            folder,
            nested: Some(inner),
        } => format!(
            "won't load   {folder}: the addon is one level too deep; move {folder}/{inner} into AddOns"
        ),
        Issue::NoToc {
            folder,
            nested: None,
        } => {
            format!("won't load   {folder}: no TOC file for this client")
        }
        Issue::Duplicate { title, folders } => {
            format!("duplicate    {title}: {}", folders.join(", "))
        }
        Issue::MissingDep { dep, needed_by } => {
            format!("missing dep  {dep}: needed by {}", needed_by.join(", "))
        }
    }
}

fn print_installed(i: &Installed) {
    match &i.previous {
        Some(prev) if *prev != i.entry.version => {
            println!(
                "updated  {} {prev} -> {}",
                i.entry.display_name(),
                i.entry.version
            );
        }
        _ => println!("installed {} {}", i.entry.display_name(), i.entry.version),
    }
    if let Some(w) = &i.warning {
        eprintln!("warning: {w}");
    }
}

fn report_check_errors(checks: &[UpdateCheck]) {
    for c in checks {
        if let Some(e) = &c.error {
            eprintln!("warning: {}: {e}", c.entry.display_name());
        }
    }
}

/// One name from `kodo remove`: what was removed, or why not.
#[derive(Serialize)]
struct Removed {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed: Option<LockEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct Row {
    name: String,
    id: AddonRef,
    installed: String,
    latest: String,
}

impl From<&UpdateCheck> for Row {
    fn from(c: &UpdateCheck) -> Self {
        Self {
            name: c.entry.display_name().to_owned(),
            id: c.entry.id.clone(),
            installed: c.entry.version.clone(),
            latest: c
                .latest
                .as_ref()
                .map(|r| r.version.clone())
                .unwrap_or_default(),
        }
    }
}

impl Row {
    fn print(&self) {
        println!(
            "{:<32} {:<16} -> {:<16} {}",
            self.name, self.installed, self.latest, self.id
        );
    }
}

/// Download progress bars; silent in JSON mode or when stderr isn't a terminal.
struct Bars(Option<MultiProgress>);

impl Bars {
    fn new(json: bool) -> Self {
        Self((!json).then(MultiProgress::new))
    }

    fn add(&self, name: &str) -> Option<Progress> {
        let multi = self.0.as_ref()?;
        let style = ProgressStyle::with_template("{prefix:<28} {bar:30} {bytes}/{total_bytes}")
            .unwrap_or_else(|_| ProgressStyle::default_bar());
        let bar = multi.add(
            ProgressBar::new(0)
                .with_style(style)
                .with_prefix(name.to_owned()),
        );
        bar.enable_steady_tick(Duration::from_millis(200));
        Some(Arc::new(move |done, total| {
            if let Some(t) = total {
                bar.set_length(t);
            }
            bar.set_position(done);
        }))
    }

    fn println(&self, line: &str) {
        match &self.0 {
            Some(m) => {
                let _ = m.println(line);
            }
            None => eprintln!("{line}"),
        }
    }

    fn clear(&self) {
        if let Some(m) = &self.0 {
            let _ = m.clear();
        }
    }
}

fn pick<'a>(clients: &'a [Client], id: Option<&str>) -> anyhow::Result<&'a Client> {
    let matching: Vec<&Client> = match id {
        Some(id) => clients
            .iter()
            .filter(|c| c.flavor.id().eq_ignore_ascii_case(id) || c.name.eq_ignore_ascii_case(id))
            .collect(),
        None => {
            let forever: Vec<_> = clients
                .iter()
                .filter(|c| c.flavor == Flavor::Forever)
                .collect();
            if forever.is_empty() {
                clients.iter().collect()
            } else {
                forever
            }
        }
    };
    match matching.as_slice() {
        [one] => Ok(one),
        [] => bail!(kodo_core::Error::BadRef(
            "no matching client; run `kodo clients` or pass --path".into()
        )),
        many => bail!(kodo_core::Error::BadRef(format!(
            "{} clients match; pass --client or --path to pick one:\n{}",
            many.len(),
            many.iter()
                .map(|c| format!("  {} {}", c.name, c.addons.display()))
                .collect::<Vec<_>>()
                .join("\n")
        ))),
    }
}
