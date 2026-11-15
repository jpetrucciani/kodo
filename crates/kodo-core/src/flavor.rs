//! Game flavors (Forever, Retail, ...) and everything that varies between them.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
pub enum Flavor {
    Forever,
    Retail,
    ClassicEra,
    Classic,
    /// Any other `_<name>_` client folder (PTR, beta, ...), keyed by `<name>`.
    Other(String),
}

struct Info {
    flavor: Flavor,
    /// Config id; the client folder is `_<id>_`.
    id: &'static str,
    label: &'static str,
    /// `Product` column in `.build.info`.
    product: &'static str,
    /// Flavor TOC suffixes, most preferred first. The plain `Name.toc` is always the last resort.
    toc_suffixes: &'static [&'static str],
    /// TOC `Interface` numbers that belong to this flavor.
    interfaces: RangeInclusive<u32>,
    /// `metadata[].flavor` strings in a packager `release.json`.
    release_flavors: &'static [&'static str],
    /// Lowercase markers in a release zip's file name that tag it for this flavor.
    zip_markers: &'static [&'static str],
    /// `game-track-list` values in the strongbox catalogue.
    catalog_tracks: &'static [&'static str],
}

/// The one table of per-flavor facts. Forever's folder, product, TOC suffix, interface and
/// `release.json` flavor are confirmed against a real client and real releases (Nov 2026).
const TABLE: [Info; 4] = [
    Info {
        flavor: Flavor::Forever,
        id: "forever",
        label: "Forever",
        product: "wowf",
        toc_suffixes: &["Camelot", "Mainline"],
        interfaces: 16000..=16999,
        release_flavors: &["forever"],
        zip_markers: &["camelot", "forever"],
        catalog_tracks: &["forever"],
    },
    Info {
        flavor: Flavor::Retail,
        id: "retail",
        label: "Retail",
        product: "wow",
        toc_suffixes: &["Mainline"],
        interfaces: 100_000..=999_999,
        release_flavors: &["mainline", "retail"],
        zip_markers: &["mainline", "retail"],
        catalog_tracks: &["retail"],
    },
    Info {
        flavor: Flavor::ClassicEra,
        id: "classic_era",
        label: "Classic Era",
        product: "wow_classic_era",
        toc_suffixes: &["Vanilla", "Classic"],
        interfaces: 11000..=11999,
        release_flavors: &["classic", "vanilla"],
        zip_markers: &["vanilla", "classic"],
        catalog_tracks: &["classic"],
    },
    Info {
        flavor: Flavor::Classic,
        id: "classic",
        label: "Classic",
        product: "wow_classic",
        toc_suffixes: &["Mists", "Classic"],
        interfaces: 20000..=99999,
        release_flavors: &["mists"],
        zip_markers: &["mists"],
        catalog_tracks: &["classic-mists"],
    },
];

impl Flavor {
    fn info(&self) -> Option<&'static Info> {
        TABLE.iter().find(|i| i.flavor == *self)
    }

    pub fn from_id(id: &str) -> Self {
        TABLE
            .iter()
            .find(|i| i.id.eq_ignore_ascii_case(id))
            .map_or_else(|| Self::Other(id.to_owned()), |i| i.flavor.clone())
    }

    /// Parse a client folder name like `_forever_`.
    pub fn from_folder(name: &str) -> Option<Self> {
        let id = name.strip_prefix('_')?.strip_suffix('_')?;
        (!id.is_empty()).then(|| Self::from_id(id))
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Other(id) => id,
            known => known.info().map_or("", |i| i.id),
        }
    }

    pub fn label(&self) -> &str {
        self.info().map_or_else(|| self.id(), |i| i.label)
    }

    pub fn product(&self) -> Option<&'static str> {
        self.info().map(|i| i.product)
    }

    pub fn toc_suffixes(&self) -> &'static [&'static str] {
        self.info().map_or(&[], |i| i.toc_suffixes)
    }

    pub fn interfaces(&self) -> Option<RangeInclusive<u32>> {
        self.info().map(|i| i.interfaces.clone())
    }

    pub fn release_flavors(&self) -> &'static [&'static str] {
        self.info().map_or(&[], |i| i.release_flavors)
    }

    pub fn zip_markers(&self) -> &'static [&'static str] {
        self.info().map_or(&[], |i| i.zip_markers)
    }

    /// Strongbox catalogue game tracks; empty for flavors it doesn't know (no filtering).
    pub fn catalog_tracks(&self) -> &'static [&'static str] {
        self.info().map_or(&[], |i| i.catalog_tracks)
    }

    /// Every known flavor's zip markers; a zip carrying none of them is the untagged default.
    pub fn all_zip_markers() -> impl Iterator<Item = &'static str> {
        TABLE.iter().flat_map(|i| i.zip_markers.iter().copied())
    }

    /// Sort key that keeps known flavors in table order, then others by id.
    pub fn sort_key(&self) -> (usize, &str) {
        let rank = TABLE
            .iter()
            .position(|i| i.flavor == *self)
            .unwrap_or(TABLE.len());
        (rank, self.id())
    }
}

impl From<Flavor> for String {
    fn from(f: Flavor) -> Self {
        f.id().to_owned()
    }
}

impl From<String> for Flavor {
    fn from(s: String) -> Self {
        Self::from_id(&s)
    }
}

#[cfg(test)]
mod tests {
    use crate::flavor::Flavor;

    #[test]
    fn folder_names_round_trip() {
        assert_eq!(Flavor::from_folder("_forever_"), Some(Flavor::Forever));
        assert_eq!(
            Flavor::from_folder("_classic_era_"),
            Some(Flavor::ClassicEra)
        );
        assert_eq!(
            Flavor::from_folder("_ptr_"),
            Some(Flavor::Other("ptr".into()))
        );
        assert_eq!(Flavor::from_folder("__"), None);
        assert_eq!(Flavor::from_folder("Interface"), None);
        assert_eq!(Flavor::Forever.id(), "forever");
        assert_eq!(Flavor::Other("ptr".into()).label(), "ptr");
    }
}
