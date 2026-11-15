//! Folders currently installed in a client's `Interface/AddOns`.

use serde::Serialize;

use crate::detect::Client;
use crate::toc::Toc;
use crate::{Result, io_err};

#[derive(Debug, Clone, Serialize)]
pub struct LocalAddon {
    pub folder: String,
    /// The TOC this client would load, or `None` if the folder has none for its flavor.
    pub toc_file: Option<String>,
    pub toc: Option<Toc>,
    pub out_of_date: bool,
}

/// List every folder in the client's AddOns dir with its selected TOC, sorted by name.
pub fn scan(client: &Client) -> Result<Vec<LocalAddon>> {
    let dir = &client.addons;
    let mut addons = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(io_err("read", dir))? {
        let entry = entry.map_err(io_err("read", dir))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let toc_path = crate::toc::select(&path, &client.flavor)?;
        let toc = toc_path.as_deref().map(Toc::read).transpose()?;
        let out_of_date = match (&toc, client.interface) {
            (Some(toc), Some(current)) => toc.is_out_of_date(&client.flavor, current),
            _ => false,
        };
        addons.push(LocalAddon {
            folder: entry.file_name().to_string_lossy().into_owned(),
            toc_file: toc_path
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            toc,
            out_of_date,
        });
    }
    addons.sort_by_cached_key(|a| a.folder.to_lowercase());
    Ok(addons)
}

#[cfg(test)]
mod tests {
    use crate::detect::scan_root;
    use crate::fixtures;
    use crate::flavor::Flavor;
    use crate::local::scan;

    #[test]
    fn scans_fixture_addons() {
        let clients = scan_root(&fixtures().join("wow")).unwrap();
        let forever = clients
            .iter()
            .find(|c| c.flavor == Flavor::Forever)
            .unwrap();
        let addons = scan(forever).unwrap();
        let folders: Vec<_> = addons.iter().map(|a| a.folder.as_str()).collect();
        assert_eq!(folders, ["MainlineOnly", "MultiToc", "NoToc"]);

        let multi = &addons[1];
        assert_eq!(multi.toc_file.as_deref(), Some("MultiToc_Camelot.toc"));
        assert_eq!(
            multi.toc.as_ref().and_then(|t| t.title.as_deref()),
            Some("Multi Toc")
        );
        assert!(!multi.out_of_date);
        assert!(
            addons[0].out_of_date,
            "MainlineOnly lists only a retail interface"
        );
        assert!(addons[2].toc.is_none());
    }
}
