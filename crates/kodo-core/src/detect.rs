//! Find WoW installs and the game clients inside them.
//!
//! An install root holds `.build.info` and one `_<flavor>_` folder per client. Detection is
//! best-effort over a bounded set of known locations; users can always add a path by hand.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::build_info::{self, BuildEntry};
use crate::flavor::Flavor;
use crate::{Error, Result, io_err};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Client {
    /// Display name; the flavor label unless renamed in config.
    pub name: String,
    pub flavor: Flavor,
    /// Install root (the folder holding `.build.info`), canonicalized.
    pub root: PathBuf,
    /// `<root>/_<flavor>_/Interface/AddOns`. May not exist yet on a fresh client.
    pub addons: PathBuf,
    pub product: Option<String>,
    pub version: Option<String>,
    pub interface: Option<u32>,
}

impl Client {
    /// The `_<flavor>_` folder holding the game executable, `Interface` and `WTF`.
    pub fn dir(&self) -> PathBuf {
        self.addons
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| self.root.clone(), Path::to_path_buf)
    }

    /// Saved settings: `WTF/Account/<account>[/<realm>/<character>]/SavedVariables`.
    pub fn wtf(&self) -> PathBuf {
        self.dir().join("WTF")
    }

    /// Whether this client's game is running, so files under it shouldn't change: the game
    /// reads AddOns at load and writes SavedVariables on logout. Best effort; `false` when the
    /// OS can't tell.
    pub fn game_running(&self) -> bool {
        let dir = self.dir();
        let exes = game_exes(&dir);
        !exes.is_empty() && running::any(&dir, &exes)
    }
}

#[derive(Debug, Default)]
pub struct Detected {
    pub clients: Vec<Client>,
    /// Locations that exist but could not be read.
    pub errors: Vec<Error>,
}

/// Scan this OS's known install locations.
pub fn detect() -> Detected {
    let mut found = Detected::default();
    for root in candidate_roots() {
        if !root.is_dir() {
            continue;
        }
        match scan_root(&root) {
            Ok(clients) => found.clients.extend(clients),
            Err(e) => found.errors.push(e),
        }
    }
    dedup(&mut found.clients);
    found
}

/// Resolve a user-supplied path: an install root, a `_<flavor>_` folder, or its `Interface/AddOns`.
///
/// Anything else is refused, so a bad folder pick can never become a managed AddOns dir.
pub fn from_path(path: &Path) -> Result<Vec<Client>> {
    let path = canonical(path)?;
    if is_addons_dir(&path) {
        let flavor_dir = path.parent().and_then(Path::parent);
        if let Some((root, flavor_dir)) = flavor_dir.and_then(|f| Some((f.parent()?, f))) {
            let entries = build_info::read(root)?;
            return Ok(client_at(root, flavor_dir, &entries).into_iter().collect());
        }
    }
    if path.join(".build.info").is_file() {
        return scan_root(&path);
    }
    if let Some(root) = path.parent() {
        let entries = build_info::read(root)?;
        if let Some(client) = client_at(root, &path, &entries) {
            return Ok(vec![client]);
        }
    }
    let clients = scan_root(&path)?;
    if clients.is_empty() {
        return Err(Error::NotAClient(path));
    }
    Ok(clients)
}

/// List every `_<flavor>_` client under an install root.
pub fn scan_root(root: &Path) -> Result<Vec<Client>> {
    let root = canonical(root)?;
    let entries = build_info::read(&root)?;
    let mut clients = Vec::new();
    for entry in std::fs::read_dir(&root).map_err(io_err("read", &root))? {
        let entry = entry.map_err(io_err("read", &root))?;
        if let Some(client) = client_at(&root, &entry.path(), &entries) {
            clients.push(client);
        }
    }
    clients.sort_by(|a, b| a.flavor.sort_key().cmp(&b.flavor.sort_key()));
    Ok(clients)
}

/// Build a client from `<root>/_<flavor>_` if it holds `Interface/AddOns` or a game executable.
fn client_at(root: &Path, dir: &Path, entries: &[BuildEntry]) -> Option<Client> {
    let flavor = Flavor::from_folder(&dir.file_name()?.to_string_lossy())?;
    let interface_dir = child_ci(dir, "Interface");
    let addons = interface_dir.as_deref().and_then(|i| child_ci(i, "AddOns"));
    if addons.is_none() && !has_game_exe(dir) {
        return None;
    }
    let entry = flavor
        .product()
        .and_then(|p| entries.iter().find(|e| e.product == p));
    let version = entry.map(|e| e.version.clone());
    Some(Client {
        interface: version
            .as_deref()
            .and_then(build_info::interface_from_version),
        product: entry.map(|e| e.product.clone()),
        version,
        addons: addons.unwrap_or_else(|| {
            interface_dir
                .unwrap_or_else(|| dir.join("Interface"))
                .join("AddOns")
        }),
        root: root.to_path_buf(),
        name: flavor.label().to_owned(),
        flavor,
    })
}

fn is_addons_dir(path: &Path) -> bool {
    let mut parts = path
        .components()
        .rev()
        .map(|c| c.as_os_str().to_string_lossy());
    parts
        .next()
        .is_some_and(|p| p.eq_ignore_ascii_case("AddOns"))
        && parts
            .next()
            .is_some_and(|p| p.eq_ignore_ascii_case("Interface"))
}

/// Find a direct child directory by name, ignoring case (Wine and macOS installs vary).
fn child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_dir() {
        return Some(exact);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name) && e.path().is_dir())
        .map(|e| e.path())
}

fn has_game_exe(dir: &Path) -> bool {
    !game_exes(dir).is_empty()
}

/// `Wow*.exe` files and `*.app` bundles in a client folder.
fn game_exes(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            let n = name.to_ascii_lowercase();
            (n.starts_with("wow") && n.ends_with(".exe")) || n.ends_with(".app")
        })
        .collect()
}

/// Is one of a client's game executables running?
mod running {
    use std::path::Path;

    /// Windows won't open a running executable for writing (access denied or a sharing
    /// violation). Nothing is written; the handle is dropped at once. Since an exe can also
    /// be unwritable by permissions alone, a locked one only counts if a process with its
    /// image name is running too.
    #[cfg(windows)]
    pub fn any(dir: &Path, exes: &[String]) -> bool {
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        let locked: Vec<&String> = exes
            .iter()
            .filter(|exe| {
                matches!(
                    std::fs::OpenOptions::new().write(true).open(dir.join(exe)),
                    Err(e) if matches!(
                        e.raw_os_error(),
                        Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
                    )
                )
            })
            .collect();
        !locked.is_empty()
            && running_images()
                .iter()
                .any(|image| locked.iter().any(|exe| exe.eq_ignore_ascii_case(image)))
    }

    /// Image names of running processes, from `tasklist` (CSV output isn't localized).
    #[cfg(windows)]
    fn running_images() -> Vec<String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Ok(out) = std::process::Command::new("tasklist")
            .args(["/FO", "CSV", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split(',').next())
            .map(|name| name.trim().trim_matches('"').to_owned())
            .collect()
    }

    /// Wine and Proton show the Windows exe path in the process command line.
    #[cfg(target_os = "linux")]
    pub fn any(_dir: &Path, exes: &[String]) -> bool {
        let names: Vec<String> = exes.iter().map(|e| e.to_ascii_lowercase()).collect();
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return false;
        };
        procs.flatten().any(|p| {
            let pid = p.file_name();
            if !pid.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            let Ok(cmd) = std::fs::read(p.path().join("cmdline")) else {
                return false;
            };
            // Arguments are NUL-separated; only the program (first one) counts.
            let program = cmd.split(|&b| b == 0).next().unwrap_or_default();
            let program = String::from_utf8_lossy(program).to_ascii_lowercase();
            names.iter().any(|n| program.ends_with(n.as_str()))
        })
    }

    /// The game runs from inside its `.app` bundle in the client folder.
    #[cfg(target_os = "macos")]
    pub fn any(dir: &Path, _exes: &[String]) -> bool {
        let Ok(out) = std::process::Command::new("ps")
            .args(["-axo", "comm="])
            .output()
        else {
            return false;
        };
        let dir = dir.to_string_lossy();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l.trim().starts_with(dir.as_ref()))
    }

    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    pub fn any(_dir: &Path, _exes: &[String]) -> bool {
        false
    }
}

fn canonical(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path)
        .map(strip_verbatim)
        .map_err(io_err("resolve", path))
}

/// Windows `canonicalize` returns verbatim paths (`\\?\C:\...`, `\\?\UNC\host\share`);
/// turn them back into the ordinary form users recognise. Other paths pass through untouched.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path;
    };
    if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

fn dedup(clients: &mut Vec<Client>) {
    let key = |c: &Client| {
        let s = c.addons.to_string_lossy().into_owned();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let mut seen = std::collections::HashSet::new();
    clients.retain(|c| seen.insert(key(c)));
}

#[cfg(not(windows))]
fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

#[cfg(windows)]
fn candidate_roots() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut roots = Vec::new();
    if let Some(db) = env("ProgramData").map(|p| p.join(r"Battle.net\Agent\product.db"))
        && let Ok(bytes) = std::fs::read(db)
    {
        roots.extend(product_db_paths(&bytes));
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        roots.extend(env(var).map(|p| p.join("World of Warcraft")));
    }
    roots
}

#[cfg(target_os = "macos")]
fn candidate_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications/World of Warcraft")];
    roots.extend(home().map(|h| h.join("Applications/World of Warcraft")));
    roots
}

/// Wine, Lutris, Steam (Proton) and Bottles prefixes, one glob level deep. Never walks all of home.
#[cfg(all(unix, not(target_os = "macos")))]
fn candidate_roots() -> Vec<PathBuf> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let children = |dir: PathBuf| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    let mut prefixes = vec![home.join(".wine")];
    prefixes.extend(children(home.join("Games")));
    for steam in [
        ".local/share/Steam",
        ".steam/steam",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam",
    ] {
        let compat = home.join(steam).join("steamapps/compatdata");
        prefixes.extend(children(compat).into_iter().map(|p| p.join("pfx")));
    }
    prefixes.extend(children(
        home.join(".var/app/com.usebottles.bottles/data/bottles/bottles"),
    ));
    prefixes
        .iter()
        .flat_map(|p| {
            ["Program Files (x86)", "Program Files"]
                .map(|pf| p.join("drive_c").join(pf).join("World of Warcraft"))
        })
        .collect()
}

/// Pull install paths out of Battle.net's `product.db` (protobuf) without a schema: walk every
/// length-delimited field, recurse into nested messages, keep strings shaped like `C:/...`.
#[cfg_attr(not(windows), allow(dead_code))]
fn product_db_paths(bytes: &[u8]) -> Vec<PathBuf> {
    fn varint(buf: &mut &[u8]) -> Option<u64> {
        let mut out = 0u64;
        for shift in (0..64).step_by(7) {
            let (&b, rest) = buf.split_first()?;
            *buf = rest;
            out |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(out);
            }
        }
        None
    }
    fn walk(mut buf: &[u8], depth: u8, out: &mut Vec<PathBuf>) -> Option<()> {
        while !buf.is_empty() {
            let skip = match varint(&mut buf)? & 7 {
                0 => {
                    varint(&mut buf)?;
                    0
                }
                1 => 8,
                5 => 4,
                2 => {
                    let len = usize::try_from(varint(&mut buf)?).ok()?;
                    let field = buf.get(..len)?;
                    match std::str::from_utf8(field) {
                        Ok(s) if looks_like_windows_path(s) => out.push(PathBuf::from(s)),
                        _ if depth < 8 => {
                            let _ = walk(field, depth + 1, out);
                        }
                        _ => {}
                    }
                    len
                }
                _ => return None,
            };
            buf = buf.get(skip..)?;
        }
        Some(())
    }
    fn looks_like_windows_path(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\')
    }
    let mut out = Vec::new();
    let _ = walk(bytes, 0, &mut out);
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::Error;
    use crate::detect::{from_path, product_db_paths, scan_root, strip_verbatim};
    use crate::fixtures;
    use crate::flavor::Flavor;

    #[test]
    fn strips_windows_verbatim_prefix() {
        let strip = |s: &str| strip_verbatim(PathBuf::from(s));
        assert_eq!(strip(r"\\?\C:\Games\WoW"), PathBuf::from(r"C:\Games\WoW"));
        assert_eq!(strip(r"\\?\UNC\nas\wow"), PathBuf::from(r"\\nas\wow"));
        assert_eq!(
            strip(r"\\?\GLOBALROOT\x"),
            PathBuf::from(r"\\?\GLOBALROOT\x")
        );
        assert_eq!(strip("/opt/wow"), PathBuf::from("/opt/wow"));
    }

    #[test]
    fn scans_install_root() {
        let clients = scan_root(&fixtures().join("wow")).unwrap();
        let flavors: Vec<_> = clients.iter().map(|c| c.flavor.clone()).collect();
        assert_eq!(
            flavors,
            [Flavor::Forever, Flavor::Retail, Flavor::Other("ptr".into())]
        );

        let forever = &clients[0];
        assert_eq!(forever.product.as_deref(), Some("wowf"));
        assert_eq!(forever.version.as_deref(), Some("1.60.1.70938"));
        assert_eq!(forever.interface, Some(16001));
        assert!(forever.addons.ends_with("_forever_/Interface/AddOns"));

        let ptr = &clients[2];
        assert_eq!(
            ptr.product, None,
            "exe-only client still detected, unlabeled"
        );
        assert!(!ptr.addons.exists());
    }

    #[test]
    fn resolves_manual_paths() {
        let root = fixtures().join("wow");
        let via_addons = from_path(&root.join("_forever_/Interface/AddOns")).unwrap();
        let via_flavor = from_path(&root.join("_forever_")).unwrap();
        assert_eq!(via_addons, via_flavor);
        assert_eq!(via_addons.len(), 1);
        assert_eq!(from_path(&root).unwrap().len(), 3);
    }

    #[test]
    fn refuses_folders_outside_addons() {
        let bad = fixtures().join("wow/_forever_/Interface");
        assert!(matches!(from_path(&bad), Err(Error::NotAClient(_))));
        let bad = fixtures().join("wow/_forever_/Interface/AddOns/MultiToc");
        assert!(matches!(from_path(&bad), Err(Error::NotAClient(_))));
    }

    #[test]
    fn reads_paths_from_product_db() {
        fn field(tag: u8, payload: &[u8]) -> Vec<u8> {
            let mut v = vec![tag, u8::try_from(payload.len()).unwrap()];
            v.extend_from_slice(payload);
            v
        }
        let install = [
            field(0x0a, b"C:/Games/World of Warcraft"),
            field(0x12, b"enUS"),
        ]
        .concat();
        let product = [
            field(0x0a, b"wowf"),
            vec![0x10, 0x96, 0x01],
            field(0x1a, &install),
        ]
        .concat();
        let db = [field(0x0a, &product), field(0x0a, &product)].concat();
        assert_eq!(
            product_db_paths(&db),
            [PathBuf::from("C:/Games/World of Warcraft")]
        );
        assert!(product_db_paths(&[0xff, 0xff]).is_empty());
    }
}
