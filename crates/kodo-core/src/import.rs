//! Addon lists from elsewhere: a WowUp export string, or plain text with one addon per line
//! (a kodo ref, a GitHub or WoWInterface link, or just a name to look up).
//!
//! WowUp's export (see `addon-broker.service.ts` in WowUp) is base64 of
//! `{"client_type": "Retail", "addons": [{"id", "name", "provider_name", "version_id"}]}`, or
//! that JSON as is.

use serde::{Deserialize, Serialize};

use crate::addon::{AddonRef, SourceId};
use crate::{Error, Result};

/// One addon the list asks for: a known project, or a name to search for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Wanted {
    pub name: String,
    pub addon: Option<AddonRef>,
}

#[derive(Deserialize)]
struct WowUpExport {
    #[serde(default)]
    client_type: Option<String>,
    addons: Vec<WowUpAddon>,
}

#[derive(Deserialize)]
struct WowUpAddon {
    id: String,
    name: String,
    provider_name: String,
}

/// What a pasted list holds, and the client a WowUp export was made for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Parsed {
    pub wanted: Vec<Wanted>,
    pub client_type: Option<String>,
}

pub fn parse(text: &str) -> Result<Parsed> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::BadRef("the list is empty".into()));
    }
    if let Some(export) = wowup(text) {
        return Ok(Parsed {
            wanted: export.addons.into_iter().map(from_wowup).collect(),
            client_type: export.client_type,
        });
    }
    let wanted = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|line| match line.parse::<AddonRef>() {
            Ok(addon) => Wanted {
                name: addon.id.rsplit('/').next().unwrap_or(&addon.id).to_owned(),
                addon: Some(addon),
            },
            Err(_) => Wanted {
                name: line.to_owned(),
                addon: None,
            },
        })
        .collect();
    Ok(Parsed {
        wanted,
        client_type: None,
    })
}

fn wowup(text: &str) -> Option<WowUpExport> {
    if text.starts_with('{') {
        return serde_json::from_str(text).ok();
    }
    // A plain list never looks like one long base64 word.
    if text.contains(char::is_whitespace) && text.lines().count() > 1 && !text.contains('=') {
        return None;
    }
    let bytes = crate::profile::base64url::decode_lenient(text)?;
    serde_json::from_slice(&bytes).ok()
}

fn from_wowup(a: WowUpAddon) -> Wanted {
    let id = a.id.trim();
    let addon = match a.provider_name.as_str() {
        "WowInterface" if id.bytes().all(|b| b.is_ascii_digit()) => {
            Some(AddonRef::new(SourceId::Wowi, id))
        }
        "Curse" if id.bytes().all(|b| b.is_ascii_digit()) => {
            Some(AddonRef::new(SourceId::Curseforge, id))
        }
        "Wago" if !id.is_empty() => Some(AddonRef::new(SourceId::Wago, id)),
        // WowUp's GitHub ids are repo URLs or owner/repo; anything else falls back to the name.
        "GitHub" => id
            .parse()
            .ok()
            .or_else(|| format!("github:{id}").parse().ok()),
        _ => None,
    };
    Wanted {
        name: a.name,
        addon,
    }
}

#[cfg(test)]
mod tests {
    use crate::addon::{AddonRef, SourceId};
    use crate::import::{Wanted, parse};

    #[test]
    fn reads_wowup_exports_and_plain_lists() {
        let json = r#"{"collection_name":"WowUp_export_1","client_type":"Retail","addons":[
            {"id":"3358","name":"Details! Damage Meter","provider_name":"Curse","version_id":"5891234"},
            {"id":"https://github.com/Cidan/BetterBags","name":"BetterBags","provider_name":"GitHub"},
            {"id":"x","name":"Some Hub Addon","provider_name":"WowUpHub"}]}"#;
        let b64 = crate::profile::base64url::encode(json.as_bytes())
            .replace('-', "+")
            .replace('_', "/");
        for text in [json.to_owned(), b64] {
            let p = parse(&text).unwrap();
            assert_eq!(p.client_type.as_deref(), Some("Retail"));
            assert_eq!(
                p.wanted[0].addon,
                Some(AddonRef::new(SourceId::Curseforge, "3358"))
            );
            assert_eq!(
                p.wanted[1].addon,
                Some(AddonRef::new(SourceId::Github, "Cidan/BetterBags"))
            );
            assert_eq!(p.wanted[2].addon, None);
        }

        let p = parse("# my raid list\nDetails\nhttps://www.wowinterface.com/downloads/info3896-Atlas.html\n\ngithub:Cidan/BetterBags\n").unwrap();
        assert_eq!(
            p.wanted,
            [
                Wanted {
                    name: "Details".into(),
                    addon: None
                },
                Wanted {
                    name: "3896".into(),
                    addon: Some(AddonRef::new(SourceId::Wowi, "3896"))
                },
                Wanted {
                    name: "BetterBags".into(),
                    addon: Some(AddonRef::new(SourceId::Github, "Cidan/BetterBags"))
                },
            ]
        );
        assert!(parse("  ").is_err());
    }
}
