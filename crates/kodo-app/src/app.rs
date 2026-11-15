//! Application state, messages and the update function (Elm-style).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::advanced::widget::operation::{Outcome, Scrollable, scrollable};
use iced::advanced::widget::{Id as WidgetId, Operation, operate};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::scrollable::AbsoluteOffset;
use iced::widget::{markdown, qr_code, text_editor};
use iced::{Point, Rectangle, Size, Task, Vector, window};
use kodo_core::addon::{AddonRef, Change, Channel, Query, Release, RemoteAddon, Sort, SourceId};
use kodo_core::config::{Config, Density, Paths, Secret, Theme, WindowConfig};
use kodo_core::detect::{self, Client};
use kodo_core::engine::{
    CharacterAddons, Engine, ImportMatch, InstallOptions, Installed, SearchResults, SwitchPlan,
    SwitchReport, UpdateCheck, ZipInstalled,
};
use kodo_core::health::Issue;
use kodo_core::history::{Event, Undo};
use kodo_core::local::LocalAddon;
use kodo_core::lockfile::LockEntry;
use kodo_core::profile::{Profile, Profiles};
use kodo_core::source::Progress;
use kodo_core::wtf::Snapshot;
use kodo_sources::github::KodoRelease;

use crate::table::{self, Col, Resize, TableId};
use crate::theme::Tokens;

/// Id of the toolbar search field, for Ctrl/Cmd+F.
pub const SEARCH_ID: &str = "kodo-search";
/// Id of the main list's scrollable, so the keyboard cursor can keep its row in view.
pub const LIST_ID: &str = "kodo-list";
/// How long notifications stay up; errors stay until dismissed.
const TOAST_LIFE: Duration = Duration::from_secs(6);
const TOAST_LIFE_UNDO: Duration = Duration::from_secs(10);
const MAX_TOASTS: usize = 4;
/// A list row's height plus the gap below it, for keeping the cursor in view.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
const SEARCH_LIMIT: usize = 50;
/// How often to look for a running game (cheap: a file open or a /proc walk).
const GAME_POLL: Duration = Duration::from_secs(5);
/// How often updates are re-checked in the background when notifications are on.
const RECHECK: Duration = Duration::from_secs(6 * 60 * 60);
/// README lines rendered on an addon page; the project page has the rest.
const README_LINES: usize = 400;
/// 256×256 RGBA pixels, generated from assets/icon/kodo.png.
const APP_ICON: &[u8] = include_bytes!("../assets/icon/kodo-256.rgba");
/// Matches the Linux .desktop file name and the macOS bundle id.
#[cfg(target_os = "linux")]
const APP_ID: &str = "dev.cobi.kodo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    FirstRun,
    Installed,
    Browse,
    Updates,
    Profiles,
    Characters,
    Health,
    History,
    Settings,
}

/// In-app page for a Browse result; release and README arrive after it opens.
#[derive(Debug, Clone)]
pub struct AddonPage {
    pub hit: RemoteAddon,
    pub release: Option<Result<Release, String>>,
    /// The release's notes, parsed.
    pub notes: Vec<markdown::Item>,
    pub readme: Option<Result<Option<Vec<markdown::Item>>, String>>,
}

/// One release's notes on the Updates screen, parsed for display.
#[derive(Debug, Clone)]
pub struct Notes {
    pub version: String,
    pub date: Option<String>,
    pub md: Vec<markdown::Item>,
}

impl From<Change> for Notes {
    fn from(c: Change) -> Self {
        Self {
            md: markdown::parse(c.notes.as_deref().unwrap_or("No notes.")).collect(),
            date: c.published_at.map(|d| d.chars().take(10).collect()),
            version: c.version,
        }
    }
}

/// Parse at most `README_LINES` lines of Markdown.
/// Which addons the Characters screen lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterFilter {
    All,
    On,
    Off,
    /// On for this character but off for another, or the other way round.
    Differs,
}

/// What the digest has shown: an addon at its latest version.
fn digest_key(c: &UpdateCheck) -> Option<(AddonRef, String)> {
    Some((c.entry.id.clone(), c.latest.as_ref()?.version.clone()))
}

/// An OS notification for updates found in the background. Shown off the UI thread; a desktop
/// without a notification service just doesn't get one.
fn notify(names: &[String]) {
    let body = match names {
        [one] => format!("{one} has an update."),
        [a, b] => format!("{a} and {b} have updates."),
        [a, rest @ ..] => format!("{a} and {} more have updates.", rest.len()),
        [] => return,
    };
    std::thread::spawn(move || {
        let _ = notify_rust::Notification::new()
            .appname("kodo")
            .summary("Addon updates")
            .body(&body)
            .show();
    });
}

/// README images fetched per page.
const README_IMAGES: usize = 8;

/// Image URLs in parsed Markdown, as written and as fetchable: relative paths resolve against
/// a GitHub repo's default branch; elsewhere they're skipped.
fn image_urls(items: &[markdown::Item], addon: &AddonRef) -> Vec<(String, String)> {
    fn walk(items: &[markdown::Item], out: &mut Vec<String>) {
        for item in items {
            match item {
                markdown::Item::Image { url, .. } => out.push(url.clone()),
                markdown::Item::Quote(inner) => walk(inner, out),
                markdown::Item::List { bullets, .. } => {
                    for b in bullets {
                        let (markdown::Bullet::Point { items }
                        | markdown::Bullet::Task { items, .. }) = b;
                        walk(items, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    walk(items, &mut found);
    found
        .into_iter()
        .filter_map(|url| {
            let abs = if url.starts_with("https://") || url.starts_with("http://") {
                url.clone()
            } else if addon.source == SourceId::Github {
                format!(
                    "https://raw.githubusercontent.com/{}/HEAD/{}",
                    addon.id,
                    url.trim_start_matches("./").trim_start_matches('/')
                )
            } else {
                return None;
            };
            Some((url, abs))
        })
        .take(README_IMAGES)
        .collect()
}

fn parse_md(md: &str) -> Vec<markdown::Item> {
    let head: Vec<&str> = md.lines().take(README_LINES).collect();
    markdown::parse(&head.join("\n")).collect()
}

/// What the client's AddOns folder and lockfile hold right now.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub entries: Vec<LockEntry>,
    /// Folders no lockfile entry owns (shown as Local, never touched).
    pub local: Vec<LocalAddon>,
    /// Folders on disk whose TOC is older than the client.
    pub out_of_date: Vec<String>,
    pub present: Vec<String>,
    /// Backed-up versions per addon, newest first.
    pub backups: HashMap<AddonRef, Vec<String>>,
    /// Catalogue or TOC-header guesses for Local folders, by folder. Linking installs over them.
    pub suggestions: HashMap<String, AddonRef>,
    pub profiles: Profiles,
    /// Folders parked out of WoW's sight.
    pub disabled: Vec<String>,
    pub issues: Vec<Issue>,
    /// Saved-settings snapshots, newest first.
    pub snapshots: Vec<Snapshot>,
    pub characters: Vec<CharacterAddons>,
    /// The client's region (`US`, `EU`…), as WoW last saw it.
    pub region: Option<String>,
    /// Everything kodo changed, newest first.
    pub history: Vec<Event>,
    /// Library folders nothing installed uses.
    pub unused: Vec<String>,
    /// When each folder was last loaded (lowercased folder → Unix seconds).
    pub last_loaded: BTreeMap<String, u64>,
    /// Latest versions already shown in the "new since last time" digest.
    pub seen: BTreeMap<AddonRef, String>,
}

impl Loaded {
    pub fn is_present(&self, e: &LockEntry) -> bool {
        e.folders
            .iter()
            .any(|f| self.present.iter().any(|p| p.eq_ignore_ascii_case(f)))
    }

    pub fn is_disabled(&self, e: &LockEntry) -> bool {
        e.folders
            .iter()
            .any(|f| self.disabled.iter().any(|d| d.eq_ignore_ascii_case(f)))
    }

    pub fn toc_out_of_date(&self, e: &LockEntry) -> bool {
        e.folders
            .iter()
            .any(|f| self.out_of_date.iter().any(|p| p.eq_ignore_ascii_case(f)))
    }
}

/// A failed job, with what the UI can offer to do about it.
#[derive(Debug, Clone)]
pub struct JobError {
    pub message: String,
    /// A forced retry: replace unmanaged folders, or remove an addon others need.
    pub retry: Option<(&'static str, Box<Message>)>,
    /// The author disallows third-party downloads; open this page instead.
    pub page: Option<String>,
}

impl JobError {
    fn from_core(e: &kodo_core::Error, addon: Option<&AddonRef>) -> Self {
        use kodo_core::Error as E;
        let retry = addon.and_then(|a| match e {
            E::Unmanaged(_) => Some((
                "Replace folders (old copy kept in backups)",
                Box::new(Message::Install(a.clone(), true)),
            )),
            E::Needed { .. } => Some(("Remove anyway", Box::new(Message::Remove(a.clone(), true)))),
            _ => None,
        });
        Self {
            message: e.to_string(),
            retry,
            page: match e {
                kodo_core::Error::Unavailable { page, .. } if !page.is_empty() => {
                    Some(page.clone())
                }
                _ => None,
            },
        }
    }

    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retry: None,
            page: None,
        }
    }
}

/// A notification in the corner, with an optional action (Undo, Retry, Details).
#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub text: String,
    pub error: bool,
    pub action: Option<(&'static str, Box<Message>)>,
    /// When it goes away on its own; errors don't.
    until: Option<Instant>,
}

/// A finished operation: what to tell the user, and how to undo it.
#[derive(Debug, Clone)]
pub struct Done {
    pub text: String,
    pub undo: Option<Box<Message>>,
}

impl Done {
    fn undo(text: impl Into<String>, undo: Message) -> Self {
        Self {
            text: text.into(),
            undo: Some(Box::new(undo)),
        }
    }
}

impl From<String> for Done {
    fn from(text: String) -> Self {
        Self { text, undo: None }
    }
}

impl From<&str> for Done {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

/// A row the keyboard cursor can sit on.
#[derive(Debug, Clone)]
pub enum RowKey {
    Addon(AddonRef),
    Local,
    Hit(RemoteAddon),
}

/// Bytes downloaded so far and the total if known, per job id.
pub type ProgressMap = HashMap<u64, (u64, Option<u64>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Install,
    Update,
}

/// Running downloads and installs. Progress callbacks write into `shared`; a timer
/// subscription copies it into `snapshot` for the view.
#[derive(Debug, Default)]
pub struct Jobs {
    pub total: usize,
    pub done: usize,
    pub active: BTreeMap<u64, (JobKind, String, AddonRef)>,
    next: u64,
    shared: Arc<Mutex<ProgressMap>>,
    pub snapshot: ProgressMap,
}

impl Jobs {
    fn start(&mut self, kind: JobKind, name: String, addon: AddonRef) -> (u64, Progress) {
        self.next += 1;
        let id = self.next;
        self.total += 1;
        self.active.insert(id, (kind, name, addon));
        let shared = self.shared.clone();
        let progress: Progress = Arc::new(move |done, total| {
            if let Ok(mut map) = shared.lock() {
                map.insert(id, (done, total));
            }
        });
        (id, progress)
    }

    fn finish(&mut self, id: u64) {
        self.active.remove(&id);
        self.done += 1;
        if let Ok(mut map) = self.shared.lock() {
            map.remove(&id);
        }
        if self.active.is_empty() {
            self.total = 0;
            self.done = 0;
            self.snapshot.clear();
        }
    }

    fn refresh(&mut self) {
        if let Ok(map) = self.shared.lock() {
            self.snapshot.clone_from(&map);
        }
    }

    pub fn running(&self) -> bool {
        !self.active.is_empty()
    }

    /// A running job for this addon, and its percent done when the size is known.
    pub fn progress(&self, addon: &AddonRef) -> Option<(JobKind, Option<u64>)> {
        let (id, (kind, ..)) = self.active.iter().find(|(_, (.., a))| a == addon)?;
        let pct = self
            .snapshot
            .get(id)
            .and_then(|(done, total)| total.filter(|t| *t > 0).map(|t| done * 100 / t));
        Some((*kind, pct))
    }
}

pub struct App {
    pub paths: Paths,
    pub config: Config,
    pub engine: Option<Engine>,
    pub system_dark: bool,
    pub screen: Screen,
    pub clients: Vec<Client>,
    pub selected: usize,
    pub loaded: Loaded,
    pub loading: bool,
    pub checks: HashMap<AddonRef, UpdateCheck>,
    pub checking: bool,
    /// Instant filter for Installed and Updates.
    pub filter: String,
    pub search: String,
    search_seq: u64,
    pub source_filter: Option<SourceId>,
    pub sort: Sort,
    /// Sort and column filters per table; widths live in `config.columns`.
    pub tables: table::Tables,
    pub resizing: Option<Resize>,
    /// How many results Browse asks for; "Show more" raises it.
    pub search_limit: usize,
    pub results: Vec<RemoteAddon>,
    pub searching: bool,
    pub search_errors: Vec<String>,
    pub detail: Option<AddonRef>,
    pub page: Option<AddonPage>,
    /// Profiles screen: name for a new profile, installed addons left out of it, a pasted code.
    pub profile_name: String,
    pub profile_skip: HashSet<AddonRef>,
    pub import_code: String,
    pub switching: bool,
    /// A profile code shown as a QR code, and its label; `None` data means it's too long.
    pub qr: Option<(String, Option<qr_code::Data>)>,
    /// Updates screen: whose release notes are open, and notes fetched so far.
    pub expanded: Option<AddonRef>,
    pub changes: HashMap<AddonRef, Result<Vec<Notes>, String>>,
    /// The selected client's game is running; file changes wait in `queued` until it closes.
    pub game_running: bool,
    pub queued: Vec<Message>,
    /// Folders of addons just installed, to point out their missing dependencies.
    just_installed: Vec<String>,
    pub kodo_update: Option<KodoRelease>,
    pub menu: Option<AddonRef>,
    pub jobs: Jobs,
    /// Installs and updates finished in the current batch, summarized once it's done:
    /// name, addon, and whether it replaced an older version (undo = roll back) or was new
    /// (undo = remove).
    finished: Vec<(String, AddonRef, bool)>,
    pub last_error: Option<JobError>,
    pub error_open: bool,
    pub toasts: Vec<Toast>,
    next_toast: u64,
    /// Rows ticked for bulk actions on Installed and Updates.
    pub picked: HashSet<AddonRef>,
    /// Keyboard cursor: an index into the current screen's rows.
    pub cursor: Option<usize>,
    pub show_keys: bool,
    /// A switch waiting for confirmation, and the profile it leaves (for Undo).
    pub switch_plan: Option<Result<SwitchPlan, String>>,
    /// The Installed row under the mouse, which shows its quick actions.
    pub hovered: Option<AddonRef>,
    /// Browse: a pasted project link or .zip path to install.
    pub link_input: String,
    /// Browse: show results as cards instead of table rows.
    pub browse_cards: bool,
    /// Browse: only results the catalogue says have a build for this client.
    pub builds_only: bool,
    /// Profiles: a pasted addon list or WowUp export, and its matches once resolved.
    pub import_list: text_editor::Content,
    pub import_matches: Option<Vec<ImportMatch>>,
    pub import_skip: HashSet<usize>,
    pub resolving: bool,
    /// Characters: the one being edited, by key.
    pub character: Option<String>,
    /// Characters: which addons the list shows.
    pub character_filter: CharacterFilter,
    /// The digest toast is shown once per launch.
    digest_toasted: bool,
    /// Whether the window has focus; notifications only go out when it doesn't.
    focused: bool,
    /// Decoded README images by URL as written in the Markdown.
    pub images: HashMap<String, iced::widget::image::Handle>,
    /// First run: detected or added clients and whether to manage them.
    pub candidates: Vec<(Client, bool)>,
    pub rename: HashMap<usize, String>,
    pub cache_size: Option<u64>,
    pub secrets: Secrets,
    window: WindowConfig,
}

/// Settings view of one secret: whether it's set, whether the env var supplies it, and the
/// unsaved text typed into its field. Values are never shown or written to config.
#[derive(Debug, Clone, Default)]
pub struct SecretField {
    pub is_set: bool,
    pub from_env: bool,
    pub input: String,
}

#[derive(Debug, Clone, Default)]
pub struct Secrets {
    pub github: SecretField,
    pub curseforge: SecretField,
}

impl Secrets {
    pub fn field(&self, s: Secret) -> Option<&SecretField> {
        match s {
            Secret::GithubToken => Some(&self.github),
            Secret::CurseforgeKey => Some(&self.curseforge),
            Secret::WagoKey => None,
        }
    }

    fn field_mut(&mut self, s: Secret) -> Option<&mut SecretField> {
        match s {
            Secret::GithubToken => Some(&mut self.github),
            Secret::CurseforgeKey => Some(&mut self.curseforge),
            Secret::WagoKey => None,
        }
    }
}

/// Engine built off the UI thread (building reads secrets from the keychain).
#[derive(Clone)]
pub struct Built(Result<Engine, String>);

impl std::fmt::Debug for Built {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_ok() {
            "Built(Ok)"
        } else {
            "Built(Err)"
        })
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Nav(Screen),
    SelectClient(String),
    Loaded(Result<Loaded, String>),
    Suggested(Vec<(String, AddonRef)>),
    Filter(String),
    Search(String),
    SearchFire(u64),
    SearchDone(u64, SearchResults),
    SourceFilter(Option<SourceId>),
    TableSort(TableId, Col),
    TableFilter(TableId, Col, String),
    ResizeStart(TableId, Col),
    ResizeMove(f32),
    ResizeEnd,
    ShowMore,
    CheckUpdates,
    ChecksDone(Result<Vec<UpdateCheck>, String>),
    Install(AddonRef, bool),
    Update(AddonRef),
    UpdateAll,
    JobDone(u64, AddonRef, Result<Installed, JobError>),
    Tick,
    OpenDetail(AddonRef),
    OpenPage(RemoteAddon),
    ClosePage,
    PageRelease(AddonRef, Result<Release, String>),
    PageReadme(AddonRef, Result<Option<String>, String>),
    /// README images that downloaded, by URL as the Markdown writes it.
    ImagesLoaded(Vec<(String, Vec<u8>)>),
    // Profiles
    ProfileName(String),
    ProfileToggle(AddonRef),
    SaveProfile,
    /// Copy a profile's code, or with `None` the picked addons'.
    CopyCode(Option<String>),
    ImportCode(String),
    ImportProfile,
    /// Preview a switch; [`Message::SwitchProfile`] confirms it.
    PlanSwitch(String),
    CancelSwitch,
    SwitchProfile(String),
    Switched(Result<SwitchReport, String>),
    DeleteProfile(String),
    /// Show a profile's code (or, with `None`, the picked addons') as a QR code.
    ShowQr(Option<String>),
    CloseQr,
    // Updates
    ToggleChanges(AddonRef),
    ChangesLoaded(AddonRef, Result<Vec<Change>, String>),
    // Health and saved settings
    Disable(String),
    Enable(String),
    /// Search Browse for an addon by name (a missing dependency).
    FindAddon(String),
    BackupWtf,
    /// Restore a snapshot: everything, or with a character key just that character's settings.
    RestoreWtf(String, Option<String>),
    PollGame,
    GameState(bool),
    KodoUpdate(Option<KodoRelease>),
    ClosePanels,
    ToggleMenu(AddonRef),
    SetChannel(AddonRef, Channel),
    Pin(AddonRef, bool),
    Rollback(AddonRef),
    /// Remove; `true` even when other addons require it.
    Remove(AddonRef, bool),
    OpDone(Result<Done, JobError>),
    /// The Installed row under the mouse.
    Hover(Option<AddonRef>),
    // Install from a link or zip
    LinkInput(String),
    LinkSubmit,
    PickZip,
    InstallZip(PathBuf),
    // Browse
    BrowseCards(bool),
    BuildsOnly(bool),
    BrowseSort(Sort),
    // Importing a list
    ImportEdit(text_editor::Action),
    ResolveImport,
    ImportResolved(Vec<ImportMatch>),
    ImportToggle(usize),
    InstallImport,
    // Characters
    SelectCharacter(String),
    /// Turn folders on or off for a character (by key).
    CharacterSet(String, Vec<(String, bool)>),
    /// Load only a profile's addons on a character.
    CharacterProfile(String, String),
    CharacterFilter(CharacterFilter),
    /// Give a character (second key) the switches of another (first key).
    CharacterCopy(String, String),
    // History and digest
    UndoEvent(Undo),
    MarkSeen,
    // Appearance
    SetDensity(Density),
    SetNotify(bool),
    SetScale(f32),
    // Notifications
    DismissToast(u64),
    ToastTick,
    /// Run several messages (bulk undo).
    Many(Vec<Message>),
    // Bulk actions
    Pick(AddonRef),
    /// Tick all of these, or untick them if they all are.
    PickAll(Vec<AddonRef>),
    ClearPicks,
    BulkUpdate,
    BulkDisable,
    BulkRemove,
    // Keyboard
    ToggleShortcuts,
    OpenUrl(String),
    OpenPath(PathBuf),
    ToggleError,
    DismissError,
    // Settings
    RenameInput(usize, String),
    RenameSave(usize),
    ToggleClient(usize),
    RemoveClient(usize),
    AddFolder,
    FolderPicked(Option<PathBuf>),
    ToggleSource(SourceId),
    Concurrency(usize),
    SetTheme(Theme),
    CacheSize(u64),
    ClearDownloads,
    SecretInput(Secret, String),
    SaveSecret(Secret),
    ClearSecret(Secret),
    SecretSaved(Secret, Result<bool, String>),
    SecretsLoaded(bool, bool),
    EngineBuilt(Built),
    // First run
    ToggleCandidate(usize),
    FinishFirstRun,
    FirstScanDone(Result<usize, String>),
    // Platform
    Key(keyboard::Event),
    Window(window::Id, window::Event),
    SystemTheme(iced::theme::Mode),
}

fn build_engine(paths: &Paths, config: &Config) -> Result<Engine, String> {
    let sources = kodo_sources::build(&kodo_sources::SourcesOptions::from_config(paths, config))
        .map_err(|e| e.to_string())?;
    Ok(Engine::new(paths.clone(), sources, config.concurrency))
}

impl App {
    /// State before any async work; pure, so tests can build one.
    pub fn new(paths: Paths, config: Config) -> (Self, Option<String>) {
        let tables = table::Tables::with_sorts(&config.sorts);
        let (engine, engine_err) = match build_engine(&paths, &config) {
            Ok(e) => (Some(e), None),
            Err(e) => (None, Some(e)),
        };
        let window = config.window.unwrap_or(WindowConfig {
            width: crate::theme::WINDOW_DEFAULT.0,
            height: crate::theme::WINDOW_DEFAULT.1,
            x: None,
            y: None,
        });
        let app = Self {
            paths,
            config,
            engine,
            system_dark: true,
            screen: Screen::Installed,
            clients: Vec::new(),
            selected: 0,
            loaded: Loaded::default(),
            loading: false,
            checks: HashMap::new(),
            checking: false,
            filter: String::new(),
            search: String::new(),
            search_seq: 0,
            source_filter: None,
            sort: Sort::Relevance,
            tables,
            resizing: None,
            search_limit: SEARCH_LIMIT,
            results: Vec::new(),
            searching: false,
            search_errors: Vec::new(),
            detail: None,
            page: None,
            profile_name: String::new(),
            profile_skip: HashSet::new(),
            import_code: String::new(),
            switching: false,
            qr: None,
            expanded: None,
            changes: HashMap::new(),
            game_running: false,
            queued: Vec::new(),
            just_installed: Vec::new(),
            kodo_update: None,
            menu: None,
            jobs: Jobs::default(),
            finished: Vec::new(),
            last_error: engine_err.clone().map(JobError::plain),
            error_open: false,
            toasts: Vec::new(),
            next_toast: 0,
            picked: HashSet::new(),
            cursor: None,
            show_keys: false,
            switch_plan: None,
            hovered: None,
            link_input: String::new(),
            browse_cards: false,
            builds_only: false,
            import_list: text_editor::Content::new(),
            import_matches: None,
            import_skip: HashSet::new(),
            resolving: false,
            character: None,
            character_filter: CharacterFilter::All,
            digest_toasted: false,
            images: HashMap::new(),
            focused: true,
            candidates: Vec::new(),
            rename: HashMap::new(),
            cache_size: None,
            secrets: Secrets {
                github: SecretField {
                    from_env: Secret::GithubToken.from_env(),
                    ..SecretField::default()
                },
                curseforge: SecretField {
                    from_env: Secret::CurseforgeKey.from_env(),
                    ..SecretField::default()
                },
            },
            window,
        };
        (app, engine_err)
    }

    pub fn boot() -> (Self, Task<Message>) {
        let portable = std::env::args().any(|a| a == "--portable");
        let (paths, config, err) = match Paths::resolve(portable) {
            Ok(paths) => match Config::load(&paths.config_file) {
                Ok(c) => (paths, c, None),
                Err(e) => (paths, Config::default(), Some(e.to_string())),
            },
            Err(e) => {
                let fallback = std::env::temp_dir().join("kodo-data");
                (
                    Paths::under(&fallback),
                    Config::default(),
                    Some(e.to_string()),
                )
            }
        };
        let (mut app, _) = App::new(paths, config);
        if let Some(e) = err {
            app.fail(JobError::plain(e));
        }
        let first_run = app.config.clients.is_empty();
        let (clients, errors) = app.config.clients();
        if let Some(e) = errors.first() {
            app.fail(JobError::plain(e.to_string()));
        }
        let mut tasks = vec![
            iced::system::theme().map(Message::SystemTheme),
            Task::perform(
                async {
                    tokio::task::spawn_blocking(|| {
                        (
                            Secret::GithubToken.get().is_some(),
                            Secret::CurseforgeKey.get().is_some(),
                        )
                    })
                    .await
                    .unwrap_or_default()
                },
                |(gh, cf)| Message::SecretsLoaded(gh, cf),
            ),
        ];
        if app.config.check_on_launch {
            let cache = app.paths.source_cache();
            tasks.push(Task::perform(
                async move {
                    kodo_sources::check_kodo(&cache, env!("CARGO_PKG_VERSION"))
                        .await
                        .ok()
                        .flatten()
                },
                Message::KodoUpdate,
            ));
        }
        if first_run {
            app.screen = Screen::FirstRun;
            app.candidates = clients.into_iter().map(|c| (c, true)).collect();
        } else {
            app.clients = clients;
            tasks.push(Task::done(Message::PollGame));
            tasks.push(app.reload());
            if app.config.check_on_launch {
                tasks.push(app.check_updates());
            }
        }
        (app, Task::batch(tasks))
    }

    pub fn tokens(&self) -> Tokens {
        Tokens::for_theme(self.config.theme, self.system_dark, self.config.density)
    }

    /// Whole-window zoom from Settings.
    pub fn scale(&self) -> f32 {
        self.config.ui_scale.clamp(0.75, 2.0)
    }

    /// Addons with an update whose latest version the digest hasn't shown yet.
    pub fn digest(&self) -> Vec<&UpdateCheck> {
        let mut new: Vec<&UpdateCheck> = self
            .checks
            .values()
            .filter(|c| c.has_update())
            .filter(|c| {
                c.latest
                    .as_ref()
                    .is_some_and(|r| self.loaded.seen.get(&c.entry.id) != Some(&r.version))
            })
            .collect();
        new.sort_by_key(|c| c.entry.display_name().to_lowercase());
        new
    }

    pub fn theme(&self) -> iced::Theme {
        self.tokens().iced()
    }

    pub fn client(&self) -> Option<&Client> {
        self.clients.get(self.selected)
    }

    pub fn update_count(&self) -> usize {
        self.checks.values().filter(|c| c.has_update()).count()
    }

    pub fn entry(&self, id: &AddonRef) -> Option<&LockEntry> {
        self.loaded.entries.iter().find(|e| e.id == *id)
    }

    fn save_config(&mut self) {
        self.config.window = Some(self.window);
        self.config.sorts = self.tables.sorts();
        if let Err(e) = self.config.save(&self.paths.config_file) {
            self.fail(JobError::plain(e.to_string()));
        }
    }

    /// Rebuild sources and engine on a blocking thread (keychain reads), e.g. after a source
    /// toggle, a new key or a concurrency change.
    fn rebuild_engine(&mut self) -> Task<Message> {
        let (paths, config) = (self.paths.clone(), self.config.clone());
        Task::perform(
            async move {
                let built = tokio::task::spawn_blocking(move || build_engine(&paths, &config))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r);
                Built(built)
            },
            Message::EngineBuilt,
        )
    }

    fn reload_clients(&mut self) -> Task<Message> {
        let (clients, errors) = self.config.clients();
        if let Some(e) = errors.first() {
            self.fail(JobError::plain(e.to_string()));
        }
        self.clients = clients;
        self.selected = self.selected.min(self.clients.len().saturating_sub(1));
        self.checks.clear();
        self.reload()
    }

    /// Re-read the lockfile, AddOns folder and backups for the selected client.
    fn reload(&mut self) -> Task<Message> {
        let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned()) else {
            self.loaded = Loaded::default();
            return Task::none();
        };
        self.loading = true;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || load(&engine, &client))
                    .await
                    .map_err(|e| e.to_string())?
            },
            Message::Loaded,
        )
    }

    /// Ask the sources what the Local folders might be (a read-only scan). Failures only mean
    /// no suggestions, so they are not surfaced as errors.
    fn suggest(&mut self) -> Task<Message> {
        let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned()) else {
            return Task::none();
        };
        Task::perform(
            async move {
                let Ok(report) = engine.scan(&client, false).await else {
                    return Vec::new();
                };
                let confident = report
                    .adopted
                    .into_iter()
                    .flat_map(|e| e.folders.into_iter().map(move |f| (f, e.id.clone())));
                let guessed = report
                    .suggestions
                    .into_iter()
                    .flat_map(|m| m.folders.into_iter().map(move |f| (f, m.addon.clone())));
                confident.chain(guessed).collect()
            },
            Message::Suggested,
        )
    }

    fn check_updates(&mut self) -> Task<Message> {
        let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned()) else {
            return Task::none();
        };
        self.checking = true;
        Task::perform(
            async move {
                engine
                    .check_updates(&client)
                    .await
                    .map_err(|e| e.to_string())
            },
            Message::ChecksDone,
        )
    }

    fn run_op<F, Fut, D>(&mut self, op: F) -> Task<Message>
    where
        F: FnOnce(Engine, Client) -> Fut,
        Fut: std::future::Future<Output = Result<D, kodo_core::Error>> + Send + 'static,
        D: Into<Done>,
    {
        self.run_op_for(None, op)
    }

    /// [`App::run_op`] for one addon, so its error can offer a forced retry.
    fn run_op_for<F, Fut, D>(&mut self, addon: Option<AddonRef>, op: F) -> Task<Message>
    where
        F: FnOnce(Engine, Client) -> Fut,
        Fut: std::future::Future<Output = Result<D, kodo_core::Error>> + Send + 'static,
        D: Into<Done>,
    {
        let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned()) else {
            return Task::none();
        };
        Task::perform(
            {
                let fut = op(engine, client);
                async move {
                    fut.await
                        .map(Into::into)
                        .map_err(|e| JobError::from_core(&e, addon.as_ref()))
                }
            },
            Message::OpDone,
        )
    }

    fn start_install(&mut self, addon: AddonRef, force: bool) -> Task<Message> {
        let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned()) else {
            return Task::none();
        };
        let name = self
            .results
            .iter()
            .find(|r| r.addon == addon)
            .map_or_else(|| addon.to_string(), |r| r.name.clone());
        let (id, progress) = self.jobs.start(JobKind::Install, name, addon.clone());
        Task::perform(
            async move {
                let opts = InstallOptions {
                    force,
                    progress: Some(progress),
                    ..InstallOptions::default()
                };
                let res = engine.install(&client, &addon, opts).await;
                let res = res.map_err(|e| JobError::from_core(&e, Some(&addon)));
                (addon, res)
            },
            move |(addon, res)| Message::JobDone(id, addon, res),
        )
    }

    fn start_update(&mut self, addon: &AddonRef) -> Task<Message> {
        let (Some(engine), Some(client), Some(check)) = (
            self.engine.clone(),
            self.client().cloned(),
            self.checks.get(addon).filter(|c| c.has_update()).cloned(),
        ) else {
            return Task::none();
        };
        let (id, progress) = self.jobs.start(
            JobKind::Update,
            check.entry.display_name().to_owned(),
            addon.clone(),
        );
        let addon = addon.clone();
        Task::perform(
            async move {
                let res = engine.apply_update(&client, &check, Some(progress)).await;
                let res = res.map_err(|e| JobError::from_core(&e, Some(&addon)));
                (addon, res)
            },
            move |(addon, res)| Message::JobDone(id, addon, res),
        )
    }

    /// Search, or with an empty field browse the catalogue, for the selected client's flavor.
    fn fire_search(&mut self) -> Task<Message> {
        let Some(engine) = self.engine.clone() else {
            return Task::none();
        };
        let q = Query {
            text: self.search.trim().to_owned(),
            limit: self.search_limit,
            sort: self.sort,
        };
        let flavor = self
            .client()
            .map_or(kodo_core::flavor::Flavor::Forever, |c| c.flavor.clone());
        let (seq, only) = (self.search_seq, self.source_filter);
        self.searching = true;
        Task::perform(
            async move { engine.search(&q, &flavor, only).await },
            move |r| Message::SearchDone(seq, r),
        )
    }

    /// Start over from the first page (new text, filter, sort or client).
    fn restart_search(&mut self) -> Task<Message> {
        self.search_seq += 1;
        self.search_limit = SEARCH_LIMIT;
        self.fire_search()
    }

    pub fn col_width(&self, id: TableId, col: Col) -> f32 {
        table::width(&self.config.columns, id, col)
    }

    /// A profile's code (or the picked addons' with `None`) and a label for it.
    fn code_for(&self, profile: Option<&str>) -> Option<(String, String)> {
        let client = self.client()?;
        let profile = match profile {
            Some(name) => self.loaded.profiles.get(name)?.clone(),
            None => {
                let name = match self.profile_name.trim() {
                    "" => format!("{}'s addons", client.name),
                    n => n.to_owned(),
                };
                Profile::from_entries(&name, &self.loaded.entries, self.selection().as_deref())
            }
        };
        let label = format!("{} ({} addons)", profile.name, profile.addons.len());
        Some((label, profile.to_code(&client.flavor)))
    }

    /// Fetch release notes between the installed and latest version, once per addon.
    fn load_changes(&mut self, id: &AddonRef) -> Task<Message> {
        let (Some(engine), Some(check)) = (self.engine.clone(), self.checks.get(id).cloned())
        else {
            return Task::none();
        };
        if self.changes.contains_key(id) {
            return Task::none();
        }
        let id = id.clone();
        Task::perform(
            async move {
                let res = engine.changes(&check).await.map_err(|e| e.to_string());
                (id, res)
            },
            |(id, res)| Message::ChangesLoaded(id, res),
        )
    }

    /// Installed addons picked on the Profiles screen; `None` when every one is.
    fn selection(&self) -> Option<Vec<AddonRef>> {
        (!self.profile_skip.is_empty()).then(|| {
            self.loaded
                .entries
                .iter()
                .filter(|e| !self.profile_skip.contains(&e.id))
                .map(|e| e.id.clone())
                .collect()
        })
    }

    /// Show a Browse result's page and fetch its latest release and README.
    fn open_page(&mut self, hit: RemoteAddon) -> Task<Message> {
        let id = hit.addon.clone();
        self.page = Some(AddonPage {
            hit,
            release: None,
            notes: Vec::new(),
            readme: None,
        });
        let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned()) else {
            return Task::none();
        };
        let release = {
            let (engine, id) = (engine.clone(), id.clone());
            Task::perform(
                async move {
                    let res = engine.resolve(&client, &id, Channel::Stable, None).await;
                    (id, res.map_err(|e| e.to_string()))
                },
                |(id, res)| Message::PageRelease(id, res),
            )
        };
        let readme = Task::perform(
            async move {
                let res = engine.readme(&id).await;
                (id, res.map_err(|e| e.to_string()))
            },
            |(id, res)| Message::PageReadme(id, res),
        );
        Task::batch([release, readme])
    }

    fn measure_cache(&self) -> Task<Message> {
        let dir = self.paths.cache_dir.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || dir_size(&dir))
                    .await
                    .unwrap_or(0)
            },
            Message::CacheSize,
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if self.game_running && message.changes_files() {
            self.queued.push(message);
            self.toast(
                format!(
                    "WoW is running; {} change(s) will apply when it closes",
                    self.queued.len()
                ),
                None,
            );
            return Task::none();
        }
        match message {
            Message::Nav(screen) => {
                if screen != self.screen {
                    self.cursor = None;
                }
                self.screen = screen;
                self.menu = None;
                if screen == Screen::Settings {
                    return self.measure_cache();
                }
                if screen == Screen::Browse && self.results.is_empty() && !self.searching {
                    return self.restart_search();
                }
            }
            Message::SelectClient(name) => {
                if let Some(i) = self.clients.iter().position(|c| c.name == name) {
                    self.selected = i;
                    self.picked.clear();
                    self.cursor = None;
                    self.checks.clear();
                    self.detail = None;
                    // Results are per flavor.
                    self.results.clear();
                    let browse = self.restart_search();
                    return Task::batch([self.reload(), self.check_updates(), browse]);
                }
            }
            Message::Loaded(res) => {
                self.loading = false;
                match res {
                    Ok(l) => {
                        let has_local = !l.local.is_empty();
                        self.loaded = l;
                        self.point_out_missing_deps();
                        if has_local {
                            return self.suggest();
                        }
                    }
                    Err(e) => self.fail(JobError::plain(e)),
                }
            }
            Message::Suggested(found) => self.loaded.suggestions = found.into_iter().collect(),
            Message::Filter(s) => {
                self.filter = s;
                self.cursor = None;
            }
            Message::Search(s) => {
                self.search = s;
                self.search_seq += 1;
                self.search_limit = SEARCH_LIMIT;
                let seq = self.search_seq;
                return Task::perform(tokio::time::sleep(SEARCH_DEBOUNCE), move |()| {
                    Message::SearchFire(seq)
                });
            }
            Message::SearchFire(seq) if seq == self.search_seq => return self.fire_search(),
            Message::SearchFire(_) => {}
            Message::SearchDone(seq, results) if seq == self.search_seq => {
                self.searching = false;
                self.results = results.hits;
                self.search_errors = results
                    .errors
                    .into_iter()
                    .map(|(s, e)| format!("{}: {e}", s.label()))
                    .collect();
            }
            Message::SearchDone(..) => {}
            Message::SourceFilter(f) => {
                self.source_filter = f;
                return self.restart_search();
            }
            Message::TableSort(id, col) => {
                self.tables.get_mut(id).toggle(col);
                self.save_config();
                let view = self.tables.get(id);
                // Browse pages come from the server in its order; sort there too so paging fits.
                if id == TableId::Browse {
                    self.sort = match view.sort {
                        Some((Col::Name, _)) => Sort::Name,
                        Some((Col::Downloads, _)) => Sort::Downloads,
                        Some((Col::Updated, _)) => Sort::Updated,
                        _ => Sort::Relevance,
                    };
                    return self.restart_search();
                }
            }
            Message::TableFilter(id, col, s) => {
                self.tables.get_mut(id).filters.insert(col, s);
            }
            Message::ResizeStart(id, col) => {
                self.resizing = Some(Resize {
                    table: id,
                    col,
                    start: self.col_width(id, col),
                    anchor: None,
                });
            }
            Message::ResizeMove(x) => {
                if let Some(r) = self.resizing.as_mut()
                    && let Some(w) = r.moved(x)
                {
                    let key = table::width_key(r.table, r.col);
                    self.config.columns.insert(key, w.round());
                }
            }
            Message::ResizeEnd => {
                if self.resizing.take().is_some() {
                    self.save_config();
                }
            }
            Message::ShowMore => {
                self.search_limit += SEARCH_LIMIT;
                self.search_seq += 1;
                return self.fire_search();
            }
            Message::CheckUpdates => return self.check_updates(),
            Message::ChecksDone(res) => {
                self.checking = false;
                match res {
                    Ok(checks) => {
                        let before: HashSet<(AddonRef, String)> =
                            self.digest().into_iter().filter_map(digest_key).collect();
                        let checks: HashMap<AddonRef, UpdateCheck> = checks
                            .into_iter()
                            .map(|c| (c.entry.id.clone(), c))
                            .collect();
                        // Ticks are kept by id; loaded release notes only while the versions they
                        // span are the same.
                        let same = |old: &UpdateCheck, new: &UpdateCheck| {
                            old.entry.version == new.entry.version
                                && old.latest.as_ref().map(|r| &r.version)
                                    == new.latest.as_ref().map(|r| &r.version)
                        };
                        self.changes.retain(|id, _| {
                            self.checks
                                .get(id)
                                .zip(checks.get(id))
                                .is_some_and(|(o, n)| same(o, n))
                        });
                        self.checks = checks;
                        let fresh: Vec<String> = self
                            .digest()
                            .into_iter()
                            .filter(|c| digest_key(c).is_some_and(|k| !before.contains(&k)))
                            .map(|c| c.entry.display_name().to_owned())
                            .collect();
                        if self.config.notify && !self.focused && !fresh.is_empty() {
                            notify(&fresh);
                        }
                        let new = self.digest().len();
                        if new > 0 && !self.digest_toasted {
                            self.digest_toasted = true;
                            self.toast(
                                format!("{new} new update(s) since last time"),
                                Some(("View", Message::Nav(Screen::Updates))),
                            );
                        }
                        if let Some(id) = self.expanded.clone() {
                            return self.load_changes(&id);
                        }
                    }
                    Err(e) => self.fail(JobError::plain(e)),
                }
            }
            Message::Install(addon, force) => return self.start_install(addon, force),
            Message::Update(addon) => return self.start_update(&addon),
            Message::UpdateAll => {
                let ids: Vec<AddonRef> = self
                    .checks
                    .values()
                    .filter(|c| c.has_update())
                    .map(|c| c.entry.id.clone())
                    .collect();
                return Task::batch(
                    ids.iter()
                        .map(|id| self.start_update(id))
                        .collect::<Vec<_>>(),
                );
            }
            Message::JobDone(id, addon, res) => {
                self.jobs.finish(id);
                match res {
                    Ok(installed) => {
                        self.just_installed
                            .extend(installed.entry.folders.iter().cloned());
                        if let Some(c) = self.checks.get_mut(&addon) {
                            c.entry = installed.entry.clone();
                        }
                        if let Some(w) = installed.warning {
                            self.fail(JobError::plain(w));
                        }
                        let replaced = installed.previous.is_some();
                        self.finished.push((
                            format!(
                                "{} {}",
                                installed.entry.display_name(),
                                installed.entry.version
                            ),
                            addon,
                            replaced,
                        ));
                    }
                    Err(e) => self.fail(e),
                }
                if !self.jobs.running() {
                    self.summarize_jobs();
                }
                return self.reload();
            }
            Message::Tick => self.jobs.refresh(),
            Message::OpenPage(hit) => return self.open_page(hit),
            Message::ClosePage => self.page = None,
            Message::PageRelease(id, res) => {
                if let Some(page) = self.page.as_mut().filter(|p| p.hit.addon == id) {
                    if let Ok(r) = &res {
                        page.notes = r.changelog.as_deref().map(parse_md).unwrap_or_default();
                    }
                    page.release = Some(res);
                }
            }
            Message::PageReadme(id, res) => {
                if let Some(page) = self.page.as_mut().filter(|p| p.hit.addon == id) {
                    let readme = res.map(|r| r.as_deref().map(parse_md));
                    let wanted: Vec<(String, String)> = match &readme {
                        Ok(Some(items)) => image_urls(items, &id),
                        _ => Vec::new(),
                    };
                    page.readme = Some(readme);
                    let wanted: Vec<(String, String)> = wanted
                        .into_iter()
                        .filter(|(orig, _)| !self.images.contains_key(orig))
                        .collect();
                    if !wanted.is_empty() {
                        let cache = self.paths.cache_dir.clone();
                        return Task::perform(
                            async move {
                                let abs = wanted.iter().map(|(_, a)| a.clone()).collect();
                                let got = kodo_sources::fetch_images(&cache, abs).await;
                                got.into_iter()
                                    .filter_map(|(abs, bytes)| {
                                        let orig = wanted.iter().find(|(_, a)| *a == abs)?;
                                        Some((orig.0.clone(), bytes))
                                    })
                                    .collect()
                            },
                            Message::ImagesLoaded,
                        );
                    }
                }
            }
            Message::ImagesLoaded(got) => {
                for (url, bytes) in got {
                    self.images
                        .insert(url, iced::widget::image::Handle::from_bytes(bytes));
                }
            }
            Message::ProfileName(s) => self.profile_name = s,
            Message::ProfileToggle(id) => {
                if !self.profile_skip.remove(&id) {
                    self.profile_skip.insert(id);
                }
            }
            Message::SaveProfile => {
                let (name, only) = (self.profile_name.clone(), self.selection());
                self.profile_name.clear();
                return self.run_op(move |engine, client| async move {
                    let p = engine.save_profile(&client, &name, only).await?;
                    Ok(format!(
                        "saved profile {} ({} addons)",
                        p.name,
                        p.addons.len()
                    ))
                });
            }
            Message::CopyCode(name) => {
                if let Some((label, code)) = self.code_for(name.as_deref()) {
                    self.toast(
                        format!("copied the code for {label}; paste it to a friend"),
                        None,
                    );
                    return iced::clipboard::write(code);
                }
            }
            Message::ShowQr(name) => {
                if let Some((label, code)) = self.code_for(name.as_deref()) {
                    let data =
                        qr_code::Data::with_error_correction(code, qr_code::ErrorCorrection::Low)
                            .ok();
                    self.qr = Some((label, data));
                }
            }
            Message::CloseQr => self.qr = None,
            Message::ToggleChanges(id) => {
                if self.expanded.as_ref() == Some(&id) {
                    self.expanded = None;
                } else {
                    self.expanded = Some(id.clone());
                    return self.load_changes(&id);
                }
            }
            Message::ChangesLoaded(id, res) => {
                self.changes
                    .insert(id, res.map(|c| c.into_iter().map(Notes::from).collect()));
            }
            Message::Disable(name) => {
                return self.run_op(move |engine, client| async move {
                    let folders = engine.disable(&client, &name).await?;
                    Ok(Done::undo(
                        format!("disabled {}", folders.join(", ")),
                        Message::Enable(name),
                    ))
                });
            }
            Message::Enable(name) => {
                return self.run_op(move |engine, client| async move {
                    let folders = engine.enable(&client, &name).await?;
                    Ok(Done::undo(
                        format!("enabled {}", folders.join(", ")),
                        Message::Disable(name),
                    ))
                });
            }
            Message::Hover(id) => self.hovered = id,
            Message::LinkInput(s) => self.link_input = s,
            Message::LinkSubmit => {
                let text = self.link_input.trim().trim_matches('"').to_owned();
                let path = PathBuf::from(&text);
                if path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                {
                    self.link_input.clear();
                    return self.update(Message::InstallZip(path));
                }
                match text.parse::<AddonRef>() {
                    Ok(addon) => {
                        self.link_input.clear();
                        return self.start_install(addon, false);
                    }
                    Err(e) => self.fail(JobError::plain(e.to_string())),
                }
            }
            Message::PickZip => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Choose an addon .zip")
                            .add_filter("Zip archive", &["zip"])
                            .pick_file()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    // Cancelled: an empty batch does nothing.
                    |p| p.map_or(Message::Many(Vec::new()), Message::InstallZip),
                );
            }
            Message::InstallZip(path) => {
                return self.run_op(move |engine, client| async move {
                    let ZipInstalled { folders, replaced } =
                        engine.install_zip(&client, &path).await?;
                    let mut text = format!("installed {} (hand-installed)", folders.join(", "));
                    if !replaced.is_empty() {
                        text.push_str(&format!("; old {} kept in backups", replaced.join(", ")));
                    }
                    Ok(Done::from(text))
                });
            }
            Message::BrowseCards(on) => self.browse_cards = on,
            Message::BuildsOnly(on) => {
                self.builds_only = on;
                self.cursor = None;
            }
            Message::BrowseSort(sort) => {
                self.sort = sort;
                self.tables.get_mut(TableId::Browse).sort = None;
                return self.restart_search();
            }
            Message::ImportEdit(action) => {
                let edit = action.is_edit();
                self.import_list.perform(action);
                if edit {
                    self.import_matches = None;
                }
            }
            Message::ResolveImport => {
                let parsed = match kodo_core::import::parse(&self.import_list.text()) {
                    Ok(p) => p,
                    Err(e) => {
                        self.fail(JobError::plain(e.to_string()));
                        return Task::none();
                    }
                };
                let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned())
                else {
                    return Task::none();
                };
                self.resolving = true;
                self.import_skip.clear();
                return Task::perform(
                    async move { engine.resolve_import(&client, parsed.wanted).await },
                    Message::ImportResolved,
                );
            }
            Message::ImportResolved(matches) => {
                self.resolving = false;
                // Near misses start unticked; the user opts in.
                self.import_skip = matches
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| !m.exact || m.installed || m.addon.is_none())
                    .map(|(i, _)| i)
                    .collect();
                self.import_matches = Some(matches);
            }
            Message::ImportToggle(i) => {
                if !self.import_skip.remove(&i) {
                    self.import_skip.insert(i);
                }
            }
            Message::InstallImport => {
                let ids: Vec<AddonRef> = self
                    .import_matches
                    .take()
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| !self.import_skip.contains(i))
                    .filter_map(|(_, m)| m.addon)
                    .collect();
                self.import_list = text_editor::Content::new();
                let tasks: Vec<_> = ids
                    .into_iter()
                    .map(|id| self.start_install(id, false))
                    .collect();
                return Task::batch(tasks);
            }
            Message::SelectCharacter(key) => self.character = Some(key),
            Message::CharacterFilter(f) => self.character_filter = f,
            Message::CharacterCopy(from, to) => {
                return self.run_op(move |engine, client| async move {
                    let prev = engine.copy_character_addons(&client, &from, &to).await?;
                    let name = |k: &str| k.rsplit('/').next().unwrap_or(k).to_owned();
                    let text = format!("{} now loads what {} does", name(&to), name(&from));
                    if prev.is_empty() {
                        return Ok(Done::from(text));
                    }
                    Ok(Done::undo(
                        text,
                        Message::UndoEvent(Undo::CharacterAddons {
                            character: to,
                            states: prev,
                        }),
                    ))
                });
            }
            Message::CharacterSet(character, changes) => {
                return self.run_op(move |engine, client| async move {
                    let n = changes.len();
                    let prev = engine
                        .set_character_addons(&client, &character, changes)
                        .await?;
                    let text = match prev.len() {
                        0 => "nothing to change".to_owned(),
                        k => format!("changed {k} of {n} folder(s)"),
                    };
                    if prev.is_empty() {
                        return Ok(Done::from(text));
                    }
                    Ok(Done::undo(
                        text,
                        Message::UndoEvent(Undo::CharacterAddons {
                            character,
                            states: prev,
                        }),
                    ))
                });
            }
            Message::CharacterProfile(character, profile) => {
                return self.run_op(move |engine, client| async move {
                    let prev = engine
                        .apply_profile_to_character(&client, &character, &profile)
                        .await?;
                    let text = format!("{character} now loads {profile}");
                    if prev.is_empty() {
                        return Ok(Done::from(text));
                    }
                    Ok(Done::undo(
                        text,
                        Message::UndoEvent(Undo::CharacterAddons {
                            character,
                            states: prev,
                        }),
                    ))
                });
            }
            Message::UndoEvent(undo) => {
                return self.run_op(move |engine, client| async move {
                    Ok(Done::from(engine.undo(&client, &undo).await?))
                });
            }
            Message::MarkSeen => {
                let shown: Vec<(AddonRef, String)> = self
                    .digest()
                    .iter()
                    .filter_map(|c| Some((c.entry.id.clone(), c.latest.as_ref()?.version.clone())))
                    .collect();
                let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned())
                else {
                    return Task::none();
                };
                if let Err(e) = engine.mark_seen(&client, &shown) {
                    self.fail(JobError::plain(e.to_string()));
                }
                self.loaded.seen = engine.seen(&client);
            }
            Message::SetDensity(d) => {
                self.config.density = d;
                self.save_config();
            }
            Message::SetNotify(on) => {
                self.config.notify = on;
                self.save_config();
            }
            Message::SetScale(f) => {
                self.config.ui_scale = f;
                self.save_config();
            }
            Message::DismissToast(id) => self.toasts.retain(|t| t.id != id),
            Message::ToastTick => {
                let now = Instant::now();
                self.toasts.retain(|t| t.until.is_none_or(|u| u > now));
            }
            Message::Many(msgs) => return Task::batch(msgs.into_iter().map(Task::done)),
            Message::Pick(id) => {
                if !self.picked.remove(&id) {
                    self.picked.insert(id);
                }
            }
            Message::PickAll(ids) => {
                if ids.iter().all(|id| self.picked.contains(id)) {
                    for id in &ids {
                        self.picked.remove(id);
                    }
                } else {
                    self.picked.extend(ids);
                }
            }
            Message::ClearPicks => self.picked.clear(),
            Message::BulkUpdate => {
                let ids: Vec<AddonRef> = self
                    .take_picks()
                    .into_iter()
                    .filter(|id| self.checks.get(id).is_some_and(UpdateCheck::has_update))
                    .collect();
                let tasks: Vec<_> = ids.iter().map(|id| self.start_update(id)).collect();
                return Task::batch(tasks);
            }
            Message::BulkDisable => {
                let ids = self.take_picks();
                return self.run_op(move |engine, client| async move {
                    let mut done = Vec::new();
                    let mut failed = Vec::new();
                    for id in ids {
                        match engine.disable(&client, &id.to_string()).await {
                            Ok(_) => done.push(id),
                            Err(e) => failed.push(format!("{id}: {e}")),
                        }
                    }
                    Ok(bulk_done("disabled", done, &failed, |id| {
                        Message::Enable(id.to_string())
                    }))
                });
            }
            Message::BulkRemove => {
                let ids = self.take_picks();
                self.detail = None;
                return self.run_op(move |engine, client| async move {
                    let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
                    let mut done = Vec::new();
                    let mut failed = Vec::new();
                    for (name, res) in engine.remove_many(&client, &names, false).await? {
                        match res {
                            Ok(e) => done.push(e.id),
                            Err(e) => failed.push(format!("{name}: {e}")),
                        }
                    }
                    Ok(bulk_done("removed", done, &failed, Message::Rollback))
                });
            }
            Message::ToggleShortcuts => self.show_keys = !self.show_keys,
            Message::FindAddon(name) => {
                self.screen = Screen::Browse;
                self.page = None;
                self.search = name;
                return self.restart_search();
            }
            Message::BackupWtf => {
                return self.run_op(|engine, client| async move {
                    Ok(match engine.backup_wtf(&client).await? {
                        Some(s) => format!("backed up {} saved-settings files", s.files),
                        None => "no saved settings to back up yet".to_owned(),
                    })
                });
            }
            Message::RestoreWtf(name, character) => {
                return self.run_op(move |engine, client| async move {
                    let n = engine.restore_wtf(&client, &name, None, character).await?;
                    Ok(format!(
                        "restored {n} saved-settings files (the previous ones were backed up)"
                    ))
                });
            }
            Message::PollGame => {
                let Some(client) = self.client().cloned() else {
                    return Task::none();
                };
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || client.game_running())
                            .await
                            .unwrap_or(false)
                    },
                    Message::GameState,
                );
            }
            Message::GameState(running) => {
                let was = std::mem::replace(&mut self.game_running, running);
                if running && !was {
                    self.toast("WoW is running; addon changes wait until it closes", None);
                }
                if was && !running {
                    // The game writes saved settings on exit; then run what waited.
                    let mut tasks = vec![self.reload()];
                    tasks.extend(self.queued.drain(..).map(Task::done));
                    return Task::batch(tasks);
                }
            }
            Message::KodoUpdate(r) => self.kodo_update = r,
            Message::ImportCode(s) => self.import_code = s,
            Message::ImportProfile => match Profile::from_code(&self.import_code) {
                Ok((profile, _)) => {
                    self.import_code.clear();
                    return self.run_op(move |engine, client| async move {
                        let p = engine.add_profile(&client, profile).await?;
                        Ok(format!(
                            "imported {} ({} addons); switch to it to install them",
                            p.name,
                            p.addons.len()
                        ))
                    });
                }
                Err(e) => self.fail(JobError::plain(e.to_string())),
            },
            Message::PlanSwitch(name) => {
                let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned())
                else {
                    return Task::none();
                };
                // Reads the lockfile and stash only; cheap enough for the UI thread.
                self.switch_plan = Some(
                    engine
                        .plan_switch(&client, &name)
                        .map_err(|e| e.to_string()),
                );
            }
            Message::CancelSwitch => self.switch_plan = None,
            Message::SwitchProfile(name) => {
                let (Some(engine), Some(client)) = (self.engine.clone(), self.client().cloned())
                else {
                    return Task::none();
                };
                self.switch_plan = None;
                self.switching = true;
                self.detail = None;
                return Task::perform(
                    async move {
                        engine
                            .switch_profile(&client, &name)
                            .await
                            .map_err(|e| e.to_string())
                    },
                    Message::Switched,
                );
            }
            Message::Switched(res) => {
                self.switching = false;
                match res {
                    Ok(r) => {
                        let mut parts = Vec::new();
                        for (n, what) in [
                            (r.stashed.len(), "stashed"),
                            (r.restored.len(), "restored"),
                            (r.installed.len(), "installed"),
                        ] {
                            if n > 0 {
                                parts.push(format!("{n} {what}"));
                            }
                        }
                        let summary = if parts.is_empty() {
                            "nothing to change".to_owned()
                        } else {
                            parts.join(", ")
                        };
                        let undo = r
                            .from
                            .clone()
                            .filter(|from| !from.eq_ignore_ascii_case(&r.profile))
                            .map(|from| ("Undo", Box::new(Message::SwitchProfile(from))));
                        self.push_toast(
                            format!("switched to {}: {summary}", r.profile),
                            false,
                            undo,
                        );
                        if let Some(w) = r.warning {
                            self.fail(JobError::plain(w));
                        }
                        if !r.failed.is_empty() {
                            let why: Vec<String> =
                                r.failed.iter().map(|(n, e)| format!("{n}: {e}")).collect();
                            self.fail(JobError::plain(format!(
                                "{} addons couldn't switch over. {}",
                                r.failed.len(),
                                why.join("; ")
                            )));
                        }
                    }
                    Err(e) => self.fail(JobError::plain(e)),
                }
                return Task::batch([self.reload(), self.check_updates()]);
            }
            Message::DeleteProfile(name) => {
                return self.run_op(move |engine, client| async move {
                    let p = engine.delete_profile(&client, &name).await?;
                    Ok(format!("deleted profile {}", p.name))
                });
            }
            Message::OpenDetail(id) => {
                self.detail = Some(id);
                self.menu = None;
            }
            Message::ClosePanels => {
                self.detail = None;
                self.menu = None;
                self.error_open = false;
            }
            Message::ToggleMenu(id) => {
                self.menu = if self.menu.as_ref() == Some(&id) {
                    None
                } else {
                    Some(id)
                };
            }
            Message::SetChannel(id, channel) => {
                return self.run_op(move |engine, client| async move {
                    let e = engine
                        .set_channel(&client, &id.to_string(), channel)
                        .await?;
                    Ok(format!(
                        "{} now follows {}",
                        e.display_name(),
                        channel.as_str()
                    ))
                });
            }
            Message::Pin(id, pin) => {
                return self.run_op(move |engine, client| async move {
                    let name = id.to_string();
                    let e = if pin {
                        engine.pin(&client, &name, None).await?
                    } else {
                        engine.unpin(&client, &name).await?
                    };
                    Ok(format!(
                        "{} {}",
                        if pin { "pinned" } else { "unpinned" },
                        e.display_name()
                    ))
                });
            }
            Message::Rollback(id) => {
                return self.run_op(move |engine, client| async move {
                    let r = engine.rollback(&client, &id.to_string()).await?;
                    let settings = match r.settings {
                        0 => String::new(),
                        n => format!(" with its saved settings ({n} files)"),
                    };
                    let warning = r.warning.map(|w| format!("; {w}")).unwrap_or_default();
                    Ok(format!(
                        "restored {} {}{settings}{warning}",
                        r.entry.display_name(),
                        r.entry.version
                    ))
                });
            }
            Message::Remove(id, force) => {
                self.detail = None;
                self.menu = None;
                return self.run_op_for(Some(id.clone()), move |engine, client| async move {
                    let e = engine.remove(&client, &id.to_string(), force).await?;
                    Ok(Done::undo(
                        format!("removed {}", e.display_name()),
                        Message::Rollback(id),
                    ))
                });
            }
            Message::OpDone(res) => {
                match res {
                    Ok(done) => {
                        let undo = done.undo.map(|m| ("Undo", m));
                        self.push_toast(done.text, false, undo);
                    }
                    Err(e) => self.fail(e),
                }
                self.menu = None;
                return Task::batch([self.reload(), self.check_updates()]);
            }
            Message::OpenUrl(url) => {
                if let Err(e) = open::that_detached(&url) {
                    self.fail(JobError::plain(format!("couldn't open {url}: {e}")));
                }
            }
            Message::OpenPath(path) => {
                if let Err(e) = open::that_detached(&path) {
                    self.fail(JobError::plain(format!(
                        "couldn't open {}: {e}",
                        path.display()
                    )));
                }
            }
            Message::ToggleError => self.error_open = !self.error_open,
            Message::DismissError => {
                self.last_error = None;
                self.error_open = false;
            }
            Message::RenameInput(i, s) => {
                self.rename.insert(i, s);
            }
            Message::RenameSave(i) => {
                if let (Some(name), Some(cfg)) =
                    (self.rename.remove(&i), self.config.clients.get_mut(i))
                {
                    let name = name.trim().to_owned();
                    if !name.is_empty() {
                        cfg.name = name;
                        self.save_config();
                        return self.reload_clients();
                    }
                }
            }
            Message::ToggleClient(i) => {
                if let Some(cfg) = self.config.clients.get_mut(i) {
                    cfg.enabled = !cfg.enabled;
                    self.save_config();
                    return self.reload_clients();
                }
            }
            Message::RemoveClient(i) => {
                if i < self.config.clients.len() {
                    self.config.clients.remove(i);
                    self.save_config();
                    return self.reload_clients();
                }
            }
            Message::AddFolder => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Choose a WoW folder")
                            .pick_folder()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    Message::FolderPicked,
                );
            }
            Message::FolderPicked(Some(path)) => match detect::from_path(&path) {
                Ok(found) if self.screen == Screen::FirstRun => {
                    for c in found {
                        if !self.candidates.iter().any(|(k, _)| k.addons == c.addons) {
                            self.candidates.push((c, true));
                        }
                    }
                }
                Ok(found) => {
                    for c in &found {
                        self.config.upsert_client(c);
                    }
                    self.save_config();
                    return self.reload_clients();
                }
                Err(e) => self.fail(JobError::plain(e.to_string())),
            },
            Message::FolderPicked(None) => {}
            Message::ToggleSource(s) => {
                let flag = match s {
                    SourceId::Github => &mut self.config.sources.github,
                    SourceId::Wowi => &mut self.config.sources.wowinterface,
                    SourceId::Curseforge => &mut self.config.sources.curseforge,
                    SourceId::Wago => return Task::none(),
                };
                *flag = !*flag;
                self.save_config();
                return self.rebuild_engine();
            }
            Message::Concurrency(n) => {
                self.config.concurrency = n;
                self.save_config();
                return self.rebuild_engine();
            }
            Message::SetTheme(t) => {
                self.config.theme = t;
                self.save_config();
            }
            Message::CacheSize(n) => self.cache_size = Some(n),
            Message::ClearDownloads => {
                let dir = self.paths.downloads();
                if dir.exists()
                    && let Err(e) = std::fs::remove_dir_all(&dir)
                {
                    self.fail(JobError::plain(format!(
                        "couldn't clear {}: {e}",
                        dir.display()
                    )));
                }
                return self.measure_cache();
            }
            Message::SecretInput(secret, value) => {
                if let Some(f) = self.secrets.field_mut(secret) {
                    f.input = value;
                }
            }
            Message::ClearSecret(secret) => return save_secret(secret, String::new()),
            Message::SaveSecret(secret) => {
                let Some(f) = self.secrets.field_mut(secret) else {
                    return Task::none();
                };
                let value = std::mem::take(&mut f.input);
                return save_secret(secret, value);
            }
            Message::SecretSaved(secret, res) => match res {
                Ok(is_set) => {
                    if let Some(f) = self.secrets.field_mut(secret) {
                        f.is_set = is_set || f.from_env;
                    }
                    self.toast(
                        format!(
                            "{} {}",
                            match secret {
                                Secret::GithubToken => "GitHub token",
                                Secret::CurseforgeKey => "CurseForge key",
                                Secret::WagoKey => "Wago key",
                            },
                            if is_set {
                                "saved to the keychain"
                            } else {
                                "removed"
                            }
                        ),
                        None,
                    );
                    return self.rebuild_engine();
                }
                Err(e) => self.fail(JobError::plain(e)),
            },
            Message::SecretsLoaded(gh, cf) => {
                self.secrets.github.is_set = gh;
                self.secrets.curseforge.is_set = cf;
            }
            Message::EngineBuilt(Built(res)) => match res {
                Ok(e) => {
                    self.engine = Some(e);
                    if self.screen == Screen::Browse {
                        return self.restart_search();
                    }
                }
                Err(e) => self.fail(JobError::plain(e)),
            },
            Message::ToggleCandidate(i) => {
                if let Some((_, on)) = self.candidates.get_mut(i) {
                    *on = !*on;
                }
            }
            Message::FinishFirstRun => {
                let chosen: Vec<Client> = self
                    .candidates
                    .iter()
                    .filter(|(_, on)| *on)
                    .map(|(c, _)| c.clone())
                    .collect();
                for c in &chosen {
                    self.config.upsert_client(c);
                }
                self.save_config();
                self.screen = Screen::Installed;
                let reload = self.reload_clients();
                let Some(engine) = self.engine.clone() else {
                    return reload;
                };
                let clients = self.clients.clone();
                let scan = Task::perform(
                    async move {
                        let mut adopted = 0;
                        for c in &clients {
                            adopted += engine
                                .scan(c, true)
                                .await
                                .map_err(|e| e.to_string())?
                                .adopted
                                .len();
                        }
                        Ok(adopted)
                    },
                    Message::FirstScanDone,
                );
                return Task::batch([reload, scan]);
            }
            Message::FirstScanDone(res) => {
                match res {
                    Ok(n) => self.toast(format!("matched {n} installed addons"), None),
                    Err(e) => self.fail(JobError::plain(e)),
                }
                return Task::batch([self.reload(), self.check_updates()]);
            }
            Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                const SCREENS: [Screen; 8] = [
                    Screen::Installed,
                    Screen::Browse,
                    Screen::Updates,
                    Screen::Profiles,
                    Screen::Characters,
                    Screen::Health,
                    Screen::History,
                    Screen::Settings,
                ];
                if self.screen == Screen::FirstRun {
                    return Task::none();
                }
                match key.as_ref() {
                    Key::Named(Named::Escape) => {
                        let open = self.page.is_some()
                            || self.detail.is_some()
                            || self.menu.is_some()
                            || self.error_open
                            || self.show_keys
                            || self.switch_plan.is_some()
                            || self.qr.is_some();
                        if !open {
                            self.picked.clear();
                        }
                        self.page = None;
                        self.detail = None;
                        self.menu = None;
                        self.error_open = false;
                        self.show_keys = false;
                        self.switch_plan = None;
                        self.qr = None;
                    }
                    Key::Named(Named::F5) => return self.reload(),
                    Key::Named(Named::ArrowDown) => return self.move_cursor(1),
                    Key::Named(Named::ArrowUp) => return self.move_cursor(-1),
                    Key::Named(Named::PageDown) => return self.move_cursor(10),
                    Key::Named(Named::PageUp) => return self.move_cursor(-10),
                    Key::Named(Named::Home) => return self.move_cursor(isize::MIN / 2),
                    Key::Named(Named::End) => return self.move_cursor(isize::MAX / 2),
                    Key::Named(Named::Enter) => {
                        if let Some(msg) = self.cursor_action(CursorAction::Open) {
                            return self.update(msg);
                        }
                    }
                    Key::Named(Named::Space) => {
                        if let Some(msg) = self.cursor_action(CursorAction::Pick) {
                            return self.update(msg);
                        }
                    }
                    Key::Named(Named::Delete) => {
                        if let Some(msg) = self.cursor_action(CursorAction::Remove) {
                            return self.update(msg);
                        }
                    }
                    Key::Character("u") if !modifiers.command() => {
                        if let Some(msg) = self.cursor_action(CursorAction::Update) {
                            return self.update(msg);
                        }
                    }
                    Key::Character("?") => self.show_keys = !self.show_keys,
                    Key::Character("a") if modifiers.command() => {
                        let ids = crate::view::row_keys(self)
                            .into_iter()
                            .filter_map(|k| match k {
                                RowKey::Addon(id) => Some(id),
                                _ => None,
                            })
                            .collect();
                        return self.update(Message::PickAll(ids));
                    }
                    Key::Character(c)
                        if modifiers.command()
                            && c.len() == 1
                            && c.as_bytes()[0].is_ascii_digit() =>
                    {
                        if let Some(screen) = c
                            .parse::<usize>()
                            .ok()
                            .and_then(|n| SCREENS.get(n.wrapping_sub(1)))
                        {
                            return self.update(Message::Nav(*screen));
                        }
                    }
                    Key::Character("f") if modifiers.command() => {
                        if matches!(self.screen, Screen::Settings | Screen::FirstRun) {
                            self.screen = Screen::Browse;
                        }
                        return iced::widget::operation::focus(SEARCH_ID);
                    }
                    Key::Character("r") if modifiers.command() => return self.check_updates(),
                    _ => {}
                }
            }
            Message::Key(_) => {}
            Message::Window(id, event) => match event {
                window::Event::Moved(Point { x, y }) => {
                    self.window.x = Some(x);
                    self.window.y = Some(y);
                }
                window::Event::Resized(Size { width, height }) => {
                    self.window.width = width;
                    self.window.height = height;
                }
                window::Event::Focused => self.focused = true,
                window::Event::Unfocused => self.focused = false,
                window::Event::CloseRequested => {
                    self.save_config();
                    return window::close(id);
                }
                window::Event::FileDropped(path)
                    if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("zip")) =>
                {
                    return self.update(Message::InstallZip(path));
                }
                _ => {}
            },
            Message::SystemTheme(mode) => {
                self.system_dark = !matches!(mode, iced::theme::Mode::Light);
            }
        }
        Task::none()
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        let mut subs = vec![
            keyboard::listen().map(Message::Key),
            window::events().map(|(id, e)| Message::Window(id, e)),
            iced::system::theme_changes().map(Message::SystemTheme),
        ];
        if self.resizing.is_some() {
            subs.push(iced::event::listen_with(|event, _, _| match event {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::ResizeMove(position.x))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )) => Some(Message::ResizeEnd),
                _ => None,
            }));
        }
        if self.jobs.running() {
            subs.push(iced::time::every(Duration::from_millis(100)).map(|_| Message::Tick));
        }
        if !self.clients.is_empty() {
            subs.push(iced::time::every(GAME_POLL).map(|_| Message::PollGame));
        }
        if self.config.notify && !self.clients.is_empty() {
            subs.push(iced::time::every(RECHECK).map(|_| Message::CheckUpdates));
        }
        if self.toasts.iter().any(|t| t.until.is_some()) {
            subs.push(iced::time::every(Duration::from_millis(500)).map(|_| Message::ToastTick));
        }
        iced::Subscription::batch(subs)
    }

    /// Window settings restored from config.
    pub fn window_settings(config: &Config) -> window::Settings {
        let (w, h) = config
            .window
            .map_or(crate::theme::WINDOW_DEFAULT, |c| (c.width, c.height));
        let position = match config.window.and_then(|c| c.x.zip(c.y)) {
            Some((x, y)) => window::Position::Specific(Point::new(x, y)),
            None => window::Position::Centered,
        };
        window::Settings {
            size: Size::new(
                w.max(crate::theme::WINDOW_MIN.0),
                h.max(crate::theme::WINDOW_MIN.1),
            ),
            min_size: Some(Size::new(
                crate::theme::WINDOW_MIN.0,
                crate::theme::WINDOW_MIN.1,
            )),
            position,
            exit_on_close_request: false,
            // Window, taskbar and alt-tab icon on Windows and X11. macOS takes the dock icon
            // from the app bundle; Wayland from the .desktop file matching the app id.
            icon: window::icon::from_rgba(APP_ICON.to_vec(), 256, 256).ok(),
            #[cfg(target_os = "linux")]
            platform_specific: window::settings::PlatformSpecific {
                application_id: APP_ID.into(),
                ..Default::default()
            },
            ..window::Settings::default()
        }
    }
}

/// Widget id of the main list's row at `index`, so the cursor can find its laid-out bounds.
pub fn row_id(index: usize) -> WidgetId {
    format!("kodo-row-{index}").into()
}

/// Scrolls the main list just enough to show one row. It measures the laid-out widgets, since
/// rows aren't one height: menus, release notes and intro lines sit between them.
struct Reveal {
    row: WidgetId,
    /// The list's bounds, its content's bounds and how far it's scrolled.
    list: Option<(Rectangle, Rectangle, Vector)>,
    target: Option<Rectangle>,
}

impl<T: Send + 'static> Operation<T> for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&WidgetId>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        if id == Some(&WidgetId::new(LIST_ID)) {
            self.list = Some((bounds, content, translation));
        }
    }

    fn container(&mut self, id: Option<&WidgetId>, bounds: Rectangle) {
        if id == Some(&self.row) {
            self.target = Some(bounds);
        }
    }

    fn finish(&self) -> Outcome<T> {
        let (Some((view, content, scrolled)), Some(row)) = (self.list, self.target) else {
            return Outcome::None;
        };
        match reveal_offset(view, content, scrolled, row) {
            Some(y) => Outcome::Chain(Box::new(scrollable::scroll_to(
                WidgetId::new(LIST_ID),
                AbsoluteOffset {
                    x: None,
                    y: Some(y),
                },
            ))),
            None => Outcome::None,
        }
    }
}

/// The scroll offset that brings `row` (laid out in `content`) into `view`, if it's not
/// fully visible at the current offset.
fn reveal_offset(
    view: Rectangle,
    content: Rectangle,
    scrolled: Vector,
    row: Rectangle,
) -> Option<f32> {
    let top = row.y - content.y;
    let bottom = top + row.height;
    if top < scrolled.y {
        Some(top)
    } else if bottom > scrolled.y + view.height {
        Some(bottom - view.height)
    } else {
        None
    }
}

/// Keyboard actions on the cursor row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorAction {
    Open,
    Pick,
    Remove,
    Update,
}

/// Sum up a bulk operation, with an Undo for the ones that worked.
fn bulk_done(
    verb: &str,
    done: Vec<AddonRef>,
    failed: &[String],
    undo: impl Fn(AddonRef) -> Message,
) -> Done {
    let mut text = format!("{verb} {} addon(s)", done.len());
    if !failed.is_empty() {
        text.push_str(&format!("; {} failed: {}", failed.len(), failed.join("; ")));
    }
    if done.is_empty() {
        return text.into();
    }
    Done::undo(text, Message::Many(done.into_iter().map(undo).collect()))
}

impl App {
    /// Show a notification; it fades after a while.
    pub fn toast(&mut self, text: impl Into<String>, action: Option<(&'static str, Message)>) {
        self.push_toast(text.into(), false, action.map(|(l, m)| (l, Box::new(m))));
    }

    fn push_toast(
        &mut self,
        text: String,
        error: bool,
        action: Option<(&'static str, Box<Message>)>,
    ) {
        self.next_toast += 1;
        let life = if action.is_some() {
            TOAST_LIFE_UNDO
        } else {
            TOAST_LIFE
        };
        self.toasts.push(Toast {
            id: self.next_toast,
            text,
            error,
            action,
            until: (!error).then(|| Instant::now() + life),
        });
        let extra = self.toasts.len().saturating_sub(MAX_TOASTS);
        self.toasts.drain(..extra);
    }

    /// Show an error as a notification that stays until dismissed; its details (and any
    /// forced retry) are in the error panel.
    pub fn fail(&mut self, e: JobError) {
        let first = e.message.lines().next().unwrap_or_default().to_owned();
        let action = e
            .retry
            .clone()
            .or(Some(("Details", Box::new(Message::ToggleError))));
        self.push_toast(first, true, action);
        self.last_error = Some(e);
    }

    /// One notification for a batch of installs and updates, once the last one finishes.
    fn summarize_jobs(&mut self) {
        let done = std::mem::take(&mut self.finished);
        let undo: Vec<Message> = done
            .iter()
            .map(|(_, id, replaced)| {
                if *replaced {
                    Message::Rollback(id.clone())
                } else {
                    Message::Remove(id.clone(), true)
                }
            })
            .collect();
        let text = match done.as_slice() {
            [] => return,
            [(one, ..)] => one.clone(),
            many => {
                let names: Vec<&str> = many.iter().take(3).map(|(n, ..)| n.as_str()).collect();
                let more = many.len().saturating_sub(3);
                let tail = if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                };
                format!("{} done: {}{tail}", many.len(), names.join(", "))
            }
        };
        self.toast(text, Some(("Undo", Message::Many(undo))));
    }

    /// Ticked addons that are still installed; clears the ticks.
    fn take_picks(&mut self) -> Vec<AddonRef> {
        let picked = std::mem::take(&mut self.picked);
        self.loaded
            .entries
            .iter()
            .filter(|e| picked.contains(&e.id))
            .map(|e| e.id.clone())
            .collect()
    }

    /// Move the keyboard cursor and scroll its row into view.
    fn move_cursor(&mut self, by: isize) -> Task<Message> {
        let rows = crate::view::row_keys(self).len();
        if rows == 0 {
            self.cursor = None;
            return Task::none();
        }
        let next = match self.cursor {
            None if by < 0 => rows - 1,
            None => 0,
            Some(i) => i.saturating_add_signed(by).min(rows - 1),
        };
        self.cursor = Some(next);
        operate(Reveal {
            row: row_id(next),
            list: None,
            target: None,
        })
    }

    fn cursor_action(&self, what: CursorAction) -> Option<Message> {
        let key = crate::view::row_keys(self).into_iter().nth(self.cursor?)?;
        match (key, what, self.screen) {
            (RowKey::Addon(id), CursorAction::Open, Screen::Installed) => {
                Some(Message::OpenDetail(id))
            }
            (RowKey::Addon(id), CursorAction::Open, Screen::Updates) => {
                Some(Message::ToggleChanges(id))
            }
            (RowKey::Hit(hit), CursorAction::Open, _) => Some(Message::OpenPage(hit)),
            (RowKey::Addon(id), CursorAction::Pick, _) => Some(Message::Pick(id)),
            (RowKey::Addon(id), CursorAction::Remove, Screen::Installed) => {
                Some(Message::Remove(id, false))
            }
            (RowKey::Addon(id), CursorAction::Update, _) => self
                .checks
                .get(&id)
                .filter(|c| c.has_update())
                .map(|_| Message::Update(id)),
            _ => None,
        }
    }

    /// After an install, say which of its folders need dependencies that aren't installed.
    fn point_out_missing_deps(&mut self) {
        let fresh = std::mem::take(&mut self.just_installed);
        let deps: Vec<&str> = self
            .loaded
            .issues
            .iter()
            .filter_map(|i| match i {
                Issue::MissingDep { dep, needed_by }
                    if needed_by.iter().any(|n| fresh.contains(n)) =>
                {
                    Some(dep.as_str())
                }
                _ => None,
            })
            .collect();
        if !deps.is_empty() {
            self.toast(
                format!(
                    "also needed: {} (find them on the Health screen)",
                    deps.join(", ")
                ),
                None,
            );
        }
    }
}

impl Message {
    /// Messages that write into the game folder, which wait while the game runs.
    fn changes_files(&self) -> bool {
        matches!(
            self,
            Self::Install(..)
                | Self::Update(_)
                | Self::UpdateAll
                | Self::SwitchProfile(_)
                | Self::Rollback(_)
                | Self::Remove(..)
                | Self::Disable(_)
                | Self::Enable(_)
                | Self::RestoreWtf(..)
                | Self::CharacterCopy(..)
                | Self::BulkUpdate
                | Self::BulkDisable
                | Self::BulkRemove
                | Self::InstallZip(_)
                | Self::InstallImport
                | Self::CharacterSet(..)
                | Self::CharacterProfile(..)
                | Self::UndoEvent(_)
        )
    }
}

/// Store (or with an empty value, delete) a secret in the keychain off the UI thread.
fn save_secret(secret: Secret, value: String) -> Task<Message> {
    Task::perform(
        async move {
            let res = tokio::task::spawn_blocking(move || {
                secret.set(&value).map(|()| !value.trim().is_empty())
            })
            .await;
            match res {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        },
        move |r| Message::SecretSaved(secret, r),
    )
}

/// Blocking: lockfile, folder scan and backup listing for one client.
fn load(engine: &Engine, client: &Client) -> Result<Loaded, String> {
    let lock = engine.lockfile(client).map_err(|e| e.to_string())?;
    let folders = if client.addons.is_dir() {
        kodo_core::local::scan(client).map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };
    let issues = kodo_core::health::check(&client.addons, &folders, &lock);
    let (folders_all, lock_all) = (folders.clone(), lock.clone());
    let backups_dir = engine.paths().backups(client);
    let backups = lock
        .addons
        .iter()
        .map(|e| {
            let versions = kodo_core::install::list_backups(&backups_dir, e)
                .into_iter()
                .map(|(_, b)| b.version)
                .filter(|v| *v != e.version)
                .collect();
            (e.id.clone(), versions)
        })
        .collect();
    Ok(Loaded {
        present: folders.iter().map(|f| f.folder.clone()).collect(),
        out_of_date: folders
            .iter()
            .filter(|f| f.out_of_date)
            .map(|f| f.folder.clone())
            .collect(),
        local: folders
            .into_iter()
            .filter(|f| lock.owner_of(&f.folder).is_none())
            .collect(),
        entries: lock.addons,
        backups,
        suggestions: HashMap::new(),
        profiles: engine.profiles(client).map_err(|e| e.to_string())?,
        disabled: engine.disabled(client),
        unused: kodo_core::health::unused_libraries(&folders_all, &lock_all),
        issues,
        snapshots: engine.wtf_snapshots(client),
        characters: engine.characters(client),
        region: engine.region(client),
        history: engine.history(client),
        last_loaded: engine.last_loaded(client),
        seen: engine.seen(client),
    })
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use kodo_core::addon::AddonRef;
    use kodo_core::config::{Config, Paths};
    use kodo_core::lockfile::LockEntry;

    use iced::{Rectangle, Vector};

    use crate::app::{
        App, CursorAction, Done, JobError, MAX_TOASTS, Message, Screen, image_urls, parse_md,
        reveal_offset,
    };

    #[test]
    fn readme_images_resolve_against_the_repo() {
        let items = parse_md(
            "![shot](docs/shot.png)\n\n> ![q](./q.gif)\n\n- ![li](https://x.dev/li.png)\n",
        );
        let gh: AddonRef = "github:a/b".parse().unwrap();
        assert_eq!(
            image_urls(&items, &gh),
            [
                (
                    "docs/shot.png".to_owned(),
                    "https://raw.githubusercontent.com/a/b/HEAD/docs/shot.png".to_owned()
                ),
                (
                    "./q.gif".to_owned(),
                    "https://raw.githubusercontent.com/a/b/HEAD/q.gif".to_owned()
                ),
                (
                    "https://x.dev/li.png".to_owned(),
                    "https://x.dev/li.png".to_owned()
                ),
            ]
        );
        let wowi: AddonRef = "wowi:1".parse().unwrap();
        assert_eq!(
            image_urls(&items, &wowi).len(),
            1,
            "relative paths need a repo"
        );
    }

    #[test]
    fn reveal_scrolls_only_as_far_as_needed() {
        let rect = |y: f32, height: f32| Rectangle {
            x: 0.0,
            y,
            width: 100.0,
            height,
        };
        // A 200 px list starting at y=100, scrolled 50 px.
        let (view, content, at) = (
            rect(100.0, 200.0),
            rect(100.0, 2000.0),
            Vector::new(0.0, 50.0),
        );
        assert_eq!(reveal_offset(view, content, at, rect(200.0, 44.0)), None);
        assert_eq!(
            reveal_offset(view, content, at, rect(120.0, 44.0)),
            Some(20.0)
        );
        // A tall row (an open menu above pushed it down) is shown to its bottom edge.
        assert_eq!(
            reveal_offset(view, content, at, rect(330.0, 60.0)),
            Some(90.0)
        );
    }

    #[test]
    fn recheck_keeps_ticks_and_unchanged_notes() {
        use kodo_core::addon::{Change, Channel, Release};
        use kodo_core::engine::UpdateCheck;

        let tmp = std::env::temp_dir().join("kodo-app-recheck-test");
        let (mut app, _) = App::new(Paths::under(&tmp), Config::default());
        let check = |id: &AddonRef, latest: &str| UpdateCheck {
            entry: LockEntry::new(id.clone(), "1", vec![id.id.clone()]),
            latest: Some(Release {
                addon: id.clone(),
                name: id.id.clone(),
                version: latest.into(),
                channel: Channel::Stable,
                download_url: None,
                page_url: None,
                size: None,
                changelog: None,
                published_at: None,
            }),
            error: None,
        };
        let a: AddonRef = "github:x/a".parse().unwrap();
        let b: AddonRef = "github:x/b".parse().unwrap();
        let _ = app.update(Message::ChecksDone(Ok(vec![
            check(&a, "2"),
            check(&b, "2"),
        ])));
        app.picked.insert(a.clone());
        for id in [&a, &b] {
            let notes = vec![crate::app::Notes::from(Change {
                version: "2".into(),
                published_at: None,
                notes: None,
            })];
            app.changes.insert(id.clone(), Ok(notes));
        }

        // b got a newer release since; a didn't.
        let _ = app.update(Message::ChecksDone(Ok(vec![
            check(&a, "2"),
            check(&b, "3"),
        ])));
        assert!(app.picked.contains(&a));
        assert!(app.changes.contains_key(&a));
        assert!(!app.changes.contains_key(&b), "stale notes are refetched");
    }

    #[test]
    fn toasts_undo_ticks_and_cursor() {
        let tmp = std::env::temp_dir().join("kodo-app-toast-test");
        let (mut app, _) = App::new(Paths::under(&tmp), Config::default());
        let id: AddonRef = "github:Cidan/BetterBags".parse().unwrap();

        let _ = app.update(Message::OpDone(Ok(Done::undo(
            "removed BetterBags",
            Message::Rollback(id.clone()),
        ))));
        let toast = app.toasts.last().unwrap();
        assert!(!toast.error && toast.until.is_some());
        assert!(matches!(
            toast.action.as_ref().map(|(l, m)| (*l, &**m)),
            Some(("Undo", Message::Rollback(r))) if r == &id
        ));
        app.fail(JobError::plain("boom\nmore detail"));
        let err = app.toasts.last().unwrap();
        assert_eq!(err.text, "boom");
        assert!(
            err.error && err.until.is_none(),
            "errors stay until dismissed"
        );
        let err_id = err.id;
        let _ = app.update(Message::DismissToast(err_id));
        assert!(app.toasts.iter().all(|t| t.id != err_id));
        for i in 0..10 {
            app.toast(format!("n{i}"), None);
        }
        assert_eq!(app.toasts.len(), MAX_TOASTS);
        assert_eq!(app.toasts.last().map(|t| t.text.as_str()), Some("n9"));

        app.screen = Screen::Installed;
        app.loaded.entries = vec![LockEntry::new(id.clone(), "1", vec!["BetterBags".into()])];
        let _ = app.update(Message::PickAll(vec![id.clone()]));
        assert!(app.picked.contains(&id));
        let _ = app.update(Message::PickAll(vec![id.clone()]));
        assert!(app.picked.is_empty(), "picking all twice clears");

        let _ = app.move_cursor(5);
        assert_eq!(app.cursor, Some(0), "the cursor stays on the rows");
        assert!(matches!(
            app.cursor_action(CursorAction::Remove),
            Some(Message::Remove(r, false)) if r == id
        ));
        assert!(
            app.cursor_action(CursorAction::Update).is_none(),
            "no update known"
        );
    }
}
