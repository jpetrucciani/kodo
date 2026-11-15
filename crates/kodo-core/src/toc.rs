//! Addon `.toc` files: header parsing and per-flavor TOC selection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::flavor::Flavor;
use crate::{Result, io_err};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Toc {
    /// Display title with color codes and texture tags removed.
    pub title: Option<String>,
    pub notes: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub interfaces: Vec<u32>,
    pub dependencies: Vec<String>,
    pub optional_deps: Vec<String>,
    /// `X-*` headers (`X-Curse-Project-ID`, `X-WoWI-ID`, `X-Website`, ...), keyed as written.
    pub extra: BTreeMap<String, String>,
}

impl Toc {
    pub fn parse(text: &str) -> Self {
        let mut toc = Self::default();
        for line in text.trim_start_matches('\u{feff}').lines() {
            let Some((key, value)) = line.strip_prefix("##").and_then(|l| l.split_once(':')) else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let is = |k: &str| key.eq_ignore_ascii_case(k);
            if is("Title") {
                toc.title.get_or_insert_with(|| strip_codes(value));
            } else if is("Notes") {
                toc.notes.get_or_insert_with(|| strip_codes(value));
            } else if is("Version") {
                toc.version.get_or_insert_with(|| value.to_owned());
            } else if is("Author") {
                toc.author.get_or_insert_with(|| strip_codes(value));
            } else if is("Interface") {
                toc.interfaces = value
                    .split(',')
                    .filter_map(|v| v.trim().parse().ok())
                    .collect();
            } else if is("OptionalDeps") {
                toc.optional_deps.extend(split_list(value));
            } else if is("RequiredDeps")
                || key.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("Dep"))
            {
                // Blizzard treats any key starting with "Dep" as required dependencies.
                toc.dependencies.extend(split_list(value));
            } else if key.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("X-")) {
                toc.extra
                    .entry(key.to_owned())
                    .or_insert_with(|| value.to_owned());
            }
        }
        toc
    }

    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(io_err("read", path))?;
        Ok(Self::parse(&String::from_utf8_lossy(&bytes)))
    }

    /// True when no listed interface for this flavor reaches the client's current interface.
    pub fn is_out_of_date(&self, flavor: &Flavor, client_interface: u32) -> bool {
        let range = flavor.interfaces();
        !self
            .interfaces
            .iter()
            .filter(|i| range.as_ref().is_none_or(|r| r.contains(i)))
            .any(|&i| i >= client_interface)
    }
}

fn split_list(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Remove WoW UI escapes: `|cffRRGGBB`, `|cnNAME:`, `|r`, `|T...|t`, `|A...|a`; `||` becomes `|`.
pub fn strip_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('|') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        rest = match tail.as_bytes().first() {
            Some(b'|') => {
                out.push('|');
                &tail[1..]
            }
            _ => skip_escape(tail).unwrap_or_else(|| {
                out.push('|');
                tail
            }),
        };
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// Given the text after a `|`, return what follows a recognized escape sequence.
fn skip_escape(tail: &str) -> Option<&str> {
    match tail.as_bytes().first()? {
        b'c' if tail.as_bytes().get(1) == Some(&b'n') => tail.find(':').map(|j| &tail[j + 1..]),
        b'c' => {
            let hex = tail.get(1..9)?;
            hex.bytes()
                .all(|b| b.is_ascii_hexdigit())
                .then(|| &tail[9..])
        }
        b'r' => Some(&tail[1..]),
        b'T' => tail.find("|t").map(|j| &tail[j + 2..]),
        b'A' => tail.find("|a").map(|j| &tail[j + 2..]),
        _ => None,
    }
}

/// Pick the TOC the given flavor's client would load from an addon folder, if any.
///
/// Order: `Name_<Suffix>.toc` / `Name-<Suffix>.toc` for each flavor suffix, then `Name.toc`.
/// Matching is case-insensitive so the same rules hold on every filesystem.
pub fn select(dir: &Path, flavor: &Flavor) -> Result<Option<PathBuf>> {
    let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Ok(None);
    };
    let mut tocs = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io_err("read", dir))? {
        let entry = entry.map_err(io_err("read", dir))?;
        let file = entry.file_name().to_string_lossy().into_owned();
        if let Some(stem) = file
            .strip_suffix(".toc")
            .or_else(|| file.strip_suffix(".TOC"))
        {
            tocs.push((stem.to_owned(), entry.path()));
        }
    }
    let wanted = flavor
        .toc_suffixes()
        .iter()
        .flat_map(|s| [format!("{name}_{s}"), format!("{name}-{s}")])
        .chain([name.clone()]);
    for want in wanted {
        if let Some((_, path)) = tocs
            .iter()
            .find(|(stem, _)| stem.eq_ignore_ascii_case(&want))
        {
            return Ok(Some(path.clone()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use crate::fixtures;
    use crate::flavor::Flavor;
    use crate::toc::{Toc, select, strip_codes};

    #[test]
    fn parses_headers_with_bom_crlf_and_lists() {
        let text = "\u{feff}## Interface: 110205, 11507 ,16001\r\n\
                    ## Title: |cff33ff99Bagan|rator |TInterface\\Icons\\inv:16|t\r\n\
                    ## Title-deDE: Taschen\r\n\
                    ## Version: 1.2.3\r\n\
                    ## Author: someone\r\n\
                    ## Dependencies: LibA, LibB\r\n\
                    ## RequiredDeps: LibC\r\n\
                    ## OptionalDeps: LibD,\r\n\
                    ## X-Curse-Project-ID: 12345\r\n\
                    ## X-Website: https://github.com/owner/repo\r\n\
                    ## SavedVariables: Foo\r\n\
                    Core.lua\r\n";
        let toc = Toc::parse(text);
        assert_eq!(toc.interfaces, vec![110205, 11507, 16001]);
        assert_eq!(toc.title.as_deref(), Some("Baganator"));
        assert_eq!(toc.version.as_deref(), Some("1.2.3"));
        assert_eq!(toc.dependencies, vec!["LibA", "LibB", "LibC"]);
        assert_eq!(toc.optional_deps, vec!["LibD"]);
        assert_eq!(
            toc.extra.get("X-Curse-Project-ID").map(String::as_str),
            Some("12345")
        );
        assert_eq!(
            toc.extra.get("X-Website").map(String::as_str),
            Some("https://github.com/owner/repo")
        );
        assert_eq!(toc.extra.len(), 2);
    }

    #[test]
    fn strips_ui_escapes() {
        assert_eq!(strip_codes("|cFFFF0000Red|r Text"), "Red Text");
        assert_eq!(strip_codes("|cnGREEN_FONT_COLOR:Go|r"), "Go");
        assert_eq!(strip_codes("|A:atlas:12:12|aName"), "Name");
        assert_eq!(strip_codes("a || b | c"), "a | b | c");
        assert_eq!(strip_codes("trailing |"), "trailing |");
        assert_eq!(strip_codes("|cZZbadhex"), "|cZZbadhex");
    }

    #[test]
    fn out_of_date_only_counts_the_flavors_interfaces() {
        let toc = Toc::parse("## Interface: 110205, 16001\n");
        assert!(!toc.is_out_of_date(&Flavor::Forever, 16001));
        assert!(toc.is_out_of_date(&Flavor::Forever, 16002));
        assert!(Toc::parse("## Interface: 110205\n").is_out_of_date(&Flavor::Forever, 16001));
        assert!(Toc::parse("").is_out_of_date(&Flavor::Forever, 16001));
    }

    #[test]
    fn selects_flavor_toc_in_order() {
        let addons = fixtures().join("wow/_forever_/Interface/AddOns");
        let pick = |dir: &str, f: &Flavor| {
            select(&addons.join(dir), f)
                .unwrap()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        };
        assert_eq!(
            pick("MultiToc", &Flavor::Forever).as_deref(),
            Some("MultiToc_Camelot.toc")
        );
        assert_eq!(
            pick("MultiToc", &Flavor::Retail).as_deref(),
            Some("MultiToc_Mainline.toc")
        );
        assert_eq!(
            pick("MultiToc", &Flavor::ClassicEra).as_deref(),
            Some("MultiToc.toc")
        );
        assert_eq!(
            pick("MainlineOnly", &Flavor::Forever).as_deref(),
            Some("mainlineonly-mainline.toc")
        );
        assert_eq!(pick("MainlineOnly", &Flavor::ClassicEra), None);
        assert_eq!(pick("NoToc", &Flavor::Forever), None);
    }
}
