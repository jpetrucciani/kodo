//! `.build.info` at a WoW install root: a pipe-delimited table, one row per installed product.

use std::path::Path;

use crate::{Result, io_err};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildEntry {
    pub product: String,
    pub version: String,
}

/// Parse the table, reading columns by header name (`Name!TYPE:size`), never by position.
pub fn parse(text: &str) -> Vec<BuildEntry> {
    let mut lines = text
        .trim_start_matches('\u{feff}')
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let column = |name: &str| {
        header
            .split('|')
            .position(|c| c.split('!').next().is_some_and(|n| n.trim() == name))
    };
    let (Some(product), Some(version)) = (column("Product"), column("Version")) else {
        return Vec::new();
    };
    lines
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('|').collect();
            Some(BuildEntry {
                product: cells.get(product)?.trim().to_owned(),
                version: cells.get(version)?.trim().to_owned(),
            })
        })
        .collect()
}

/// Read `<root>/.build.info`; a missing file is an empty table.
pub fn read(root: &Path) -> Result<Vec<BuildEntry>> {
    let path = root.join(".build.info");
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(io_err("read", path)(e)),
    }
}

/// Map a client version like `1.60.1.70938` to its TOC interface number (`16001`).
pub fn interface_from_version(version: &str) -> Option<u32> {
    let mut parts = version.split('.').map(str::parse::<u32>);
    let major = parts.next()?.ok()?;
    let minor = parts.next()?.ok()?;
    let patch = parts.next()?.ok()?;
    Some(major * 10000 + minor * 100 + patch)
}

#[cfg(test)]
mod tests {
    use crate::build_info::{BuildEntry, interface_from_version, parse};

    #[test]
    fn reads_columns_by_header_name() {
        let text = "\u{feff}Branch!STRING:0|Active!DEC:1|Version!STRING:0|Build Key!HEX:16|Product!STRING:0\r\n\
                    us|1|1.60.1.70938|abc|wowf\r\n\
                    us|1|11.2.5.63906|def|wow\r\n";
        assert_eq!(
            parse(text),
            vec![
                BuildEntry {
                    product: "wowf".into(),
                    version: "1.60.1.70938".into()
                },
                BuildEntry {
                    product: "wow".into(),
                    version: "11.2.5.63906".into()
                },
            ]
        );
        assert!(parse("Branch!STRING:0\nus\n").is_empty());
    }

    #[test]
    fn interface_numbers() {
        assert_eq!(interface_from_version("1.60.1.70938"), Some(16001));
        assert_eq!(interface_from_version("11.2.5.63906"), Some(110205));
        assert_eq!(interface_from_version("1.15.7"), Some(11507));
        assert_eq!(interface_from_version("garbage"), None);
    }
}
