//! Install → update → rollback → remove → undo through the real engine and GitHub adapter,
//! against recorded-shape HTTP responses served by wiremock.

#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use kodo_core::addon::AddonRef;
use kodo_core::config::Paths;
use kodo_core::detect;
use kodo_core::engine::{Engine, InstallOptions};
use kodo_sources::catalog::Catalog;
use kodo_sources::github::GitHub;
use kodo_sources::http::Http;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn zip_bytes(version: &str) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buf);
    let opts = zip::write::SimpleFileOptions::default();
    for (name, body) in [
        (
            "Demo/Demo.toc",
            format!("## Interface: 16001\n## Title: Demo\n## Version: {version}\n"),
        ),
        ("Demo/version.txt", version.to_owned()),
    ] {
        zip.start_file(name, opts).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    buf.into_inner()
}

async fn serve_release(server: &MockServer, version: &str) {
    let zip = zip_bytes(version);
    let asset = format!("/dl/{version}/Demo-{version}.zip");
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/repos/o/Demo/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "tag_name": version,
            "name": version,
            "html_url": format!("{}/o/Demo", server.uri()),
            "assets": [{
                "name": format!("Demo-{version}.zip"),
                "size": zip.len(),
                "browser_download_url": format!("{}{asset}", server.uri()),
            }],
        }])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(asset))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(zip))
        .mount(server)
        .await;
}

fn installed_version(addons: &Path) -> String {
    std::fs::read_to_string(addons.join("Demo/version.txt")).unwrap()
}

#[tokio::test]
async fn install_update_rollback_remove_undo() {
    let server = MockServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let addons = tmp.path().join("wow/_forever_/Interface/AddOns");
    std::fs::create_dir_all(&addons).unwrap();
    let client = detect::from_path(&addons).unwrap().remove(0);
    let paths = Paths::under(&tmp.path().join("kodo"));
    let http = Arc::new(Http::new(&paths.source_cache()).unwrap());
    // Installs never consult the catalogue; point it at nothing.
    let catalog = Arc::new(Catalog::new(http.clone(), Vec::new(), &paths.cache_dir));
    let gh = GitHub::new(http, server.uri(), None, catalog);
    let engine = Engine::new(paths, vec![Arc::new(gh)], 2);
    let demo: AddonRef = "github:o/Demo".parse().unwrap();

    serve_release(&server, "v1").await;
    let first = engine
        .install(&client, &demo, InstallOptions::default())
        .await
        .unwrap();
    assert_eq!(first.entry.version, "v1");
    assert_eq!(installed_version(&addons), "v1");

    // Disabled folders sit out of WoW's sight and block updates until enabled again.
    assert_eq!(engine.disable(&client, "Demo").await.unwrap(), ["Demo"]);
    assert!(!addons.join("Demo").exists());
    assert_eq!(engine.disabled(&client), ["Demo"]);
    engine.enable(&client, "demo").await.unwrap();
    assert_eq!(installed_version(&addons), "v1");

    // Settings v1 wrote, snapshotted before the update and restored by the rollback.
    let sv = client.wtf().join("Account/ME#1/SavedVariables/Demo.lua");
    std::fs::create_dir_all(sv.parent().unwrap()).unwrap();
    std::fs::write(&sv, "Demo = { v = 1 }").unwrap();

    serve_release(&server, "v2").await;
    let checks = engine.check_updates(&client).await.unwrap();
    assert!(checks[0].has_update());
    let updated = engine
        .apply_update(&client, &checks[0], None)
        .await
        .unwrap();
    assert_eq!(
        (updated.previous.as_deref(), updated.entry.version.as_str()),
        (Some("v1"), "v2")
    );
    assert_eq!(installed_version(&addons), "v2");
    std::fs::write(&sv, "Demo = { v = 2, migrated = true }").unwrap();

    let back = engine.rollback(&client, "Demo").await.unwrap();
    assert_eq!((back.entry.version.as_str(), back.settings), ("v1", 1));
    assert_eq!(installed_version(&addons), "v1");
    assert_eq!(std::fs::read_to_string(&sv).unwrap(), "Demo = { v = 1 }");
    // The v2 settings were snapshotted before being replaced.
    assert!(engine.wtf_snapshots(&client).len() >= 2);

    // Another addon requires Demo, so removing it needs force.
    let needy = addons.join("Needy");
    std::fs::create_dir_all(&needy).unwrap();
    std::fs::write(needy.join("Needy.toc"), "## Dependencies: Demo\n").unwrap();
    let err = engine.remove(&client, "demo", false).await.unwrap_err();
    assert!(matches!(err, kodo_core::Error::Needed { .. }), "{err}");
    std::fs::remove_dir_all(&needy).unwrap();

    engine.remove(&client, "demo", false).await.unwrap();
    assert!(!addons.join("Demo").exists());
    assert!(engine.lockfile(&client).unwrap().addons.is_empty());

    let undone = engine.rollback(&client, "Demo").await.unwrap();
    assert_eq!(undone.entry.version, "v1");
    assert_eq!(installed_version(&addons), "v1");
    assert_eq!(engine.lockfile(&client).unwrap().addons.len(), 1);

    // Every change is in the history, newest first, with how to undo it.
    use kodo_core::history::Undo;
    let log = engine.history(&client);
    assert!(log[0].what.starts_with("rolled back Demo"), "{:?}", log[0]);
    assert!(log.iter().any(|e| e.what == "removed Demo"
        && e.undo
            == Some(Undo::Rollback {
                addon: demo.clone()
            })));
    assert!(log.iter().any(|e| e.what == "installed Demo v1"
        && e.undo
            == Some(Undo::Remove {
                addon: demo.clone()
            })));
    assert!(log.iter().any(|e| e.what == "disabled Demo"));

    // A zip from disk lands as a hand-installed folder; one already there is backed up.
    std::fs::create_dir_all(addons.join("Extra")).unwrap();
    let zip = tmp.path().join("Extra.zip");
    {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        z.start_file("Extra/Extra.toc", zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"## Interface: 16001\n## Title: Extra\n")
            .unwrap();
        z.finish().unwrap();
    }
    let got = engine.install_zip(&client, &zip).await.unwrap();
    assert_eq!(
        (got.folders, got.replaced),
        (vec!["Extra".to_owned()], vec!["Extra".to_owned()])
    );
    assert!(addons.join("Extra/Extra.toc").exists());
    assert!(
        engine
            .lockfile(&client)
            .unwrap()
            .get(&"github:o/Extra".parse().unwrap())
            .is_none()
    );

    // Per-character switches, and a profile applied to one character.
    let char_dir = client.wtf().join("Account/1#1/91/Me-Realm");
    std::fs::create_dir_all(&char_dir).unwrap();
    std::fs::write(char_dir.join("AddOns.txt"), "Blizzard_X: enabled\r\n").unwrap();
    let prev = engine
        .set_character_addons(&client, "me-realm", vec![("Demo".into(), false)])
        .await
        .unwrap();
    assert_eq!(prev, [("Demo".to_owned(), true)]);
    assert_eq!(
        std::fs::read_to_string(char_dir.join("AddOns.txt")).unwrap(),
        "Blizzard_X: enabled\r\nDemo: disabled\r\n"
    );
    let chars = engine.characters(&client);
    assert_eq!(chars[0].states.get("Demo"), Some(&false));
    let profile = engine.profiles(&client).unwrap().active.unwrap();
    engine
        .apply_profile_to_character(&client, "Me-Realm", &profile)
        .await
        .unwrap();
    assert_eq!(
        engine.characters(&client)[0].states.get("Demo"),
        Some(&true)
    );

    // Copy another character's switches; restore just one character's settings.
    let alt_dir = client.wtf().join("Account/1#1/91/Alt-Realm");
    std::fs::create_dir_all(alt_dir.join("SavedVariables")).unwrap();
    std::fs::write(alt_dir.join("AddOns.txt"), "Demo: disabled\r\n").unwrap();
    std::fs::write(alt_dir.join("SavedVariables/Demo.lua"), "alt = 1").unwrap();
    std::fs::create_dir_all(char_dir.join("SavedVariables")).unwrap();
    std::fs::write(char_dir.join("SavedVariables/Demo.lua"), "me = 1").unwrap();
    engine
        .copy_character_addons(&client, "Alt-Realm", "Me-Realm")
        .await
        .unwrap();
    let chars = engine.characters(&client);
    let me = chars
        .iter()
        .find(|c| c.character.name == "Me-Realm")
        .unwrap();
    assert_eq!(me.states.get("Demo"), Some(&false));
    assert!(me.saved.contains("demo"));
    assert!(me.last_played.is_some());

    let snap = engine.backup_wtf(&client).await.unwrap().unwrap();
    std::fs::write(char_dir.join("SavedVariables/Demo.lua"), "me = 2").unwrap();
    std::fs::write(alt_dir.join("SavedVariables/Demo.lua"), "alt = 2").unwrap();
    let n = engine
        .restore_wtf(&client, &snap.name, None, Some("Me-Realm".into()))
        .await
        .unwrap();
    assert_eq!(n, 1);
    let read =
        |d: &std::path::Path| std::fs::read_to_string(d.join("SavedVariables/Demo.lua")).unwrap();
    assert_eq!(read(&char_dir), "me = 1");
    assert_eq!(read(&alt_dir), "alt = 2", "other characters keep theirs");
    let me = engine.characters(&client);
    let me = me.iter().find(|c| c.character.name == "Me-Realm").unwrap();
    assert!(!me.snapshots.is_empty());
}

#[tokio::test]
async fn scan_names_bundles_after_the_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let addons = tmp.path().join("wow/_forever_/Interface/AddOns");
    let toc = |folder: &str, site: &str| {
        let dir = addons.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{folder}.toc")),
            format!("## Interface: 16001\n## Title: {folder}\n## X-Website: {site}\n"),
        )
        .unwrap();
    };
    // Two unrelated addons from one monorepo, and a real multi-folder addon.
    toc("DingDong", "https://github.com/me/wow-forever");
    toc("MoveStuff", "https://github.com/me/wow-forever");
    toc("DBM-Core", "https://github.com/DeadlyBossMods/DBM");
    toc("DBM-PvP", "https://github.com/DeadlyBossMods/DBM");
    let client = detect::from_path(&addons).unwrap().remove(0);
    let paths = Paths::under(&tmp.path().join("kodo"));
    let http = Arc::new(Http::new(&paths.source_cache()).unwrap());
    let catalog = Arc::new(Catalog::new(http.clone(), Vec::new(), &paths.cache_dir));
    let gh = GitHub::new(http, "http://127.0.0.1:9", None, catalog);
    let engine = Engine::new(paths, vec![Arc::new(gh)], 2);

    let report = engine.scan(&client, false).await.unwrap();
    let mut names: Vec<_> = report
        .adopted
        .iter()
        .map(|e| (e.display_name().to_owned(), e.folders.len()))
        .collect();
    names.sort();
    assert_eq!(names, [("DBM-Core".into(), 2), ("wow-forever".into(), 2)]);
}

#[tokio::test]
async fn profiles_stash_restore_and_install() {
    let server = MockServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let addons = tmp.path().join("wow/_forever_/Interface/AddOns");
    std::fs::create_dir_all(&addons).unwrap();
    let client = detect::from_path(&addons).unwrap().remove(0);
    let paths = Paths::under(&tmp.path().join("kodo"));
    let http = Arc::new(Http::new(&paths.source_cache()).unwrap());
    let catalog = Arc::new(Catalog::new(http.clone(), Vec::new(), &paths.cache_dir));
    let gh = GitHub::new(http, server.uri(), None, catalog);
    let engine = Engine::new(paths, vec![Arc::new(gh)], 2);
    let demo: AddonRef = "github:o/Demo".parse().unwrap();

    serve_release(&server, "v1").await;
    engine
        .install(&client, &demo, InstallOptions::default())
        .await
        .unwrap();
    // A hand-installed folder that no profile manages.
    std::fs::create_dir_all(addons.join("Mine")).unwrap();

    // The client starts on a Default profile holding what's installed.
    let set = engine.profiles(&client).unwrap();
    assert_eq!(set.active.as_deref(), Some("Default"));
    assert!(set.get("Default").is_some_and(|p| p.contains(&demo)));
    let raid = engine.save_profile(&client, "Raid", None).await.unwrap();
    engine
        .save_profile(&client, "Bare", Some(Vec::new()))
        .await
        .unwrap();

    let plan = engine.plan_switch(&client, "bare").unwrap();
    assert_eq!(
        (plan.stash.as_slice(), plan.download.len()),
        (&["Demo".to_owned()][..], 0)
    );
    let away = engine.switch_profile(&client, "bare").await.unwrap();
    assert_eq!(away.stashed, ["Demo"]);
    assert!(!addons.join("Demo").exists());
    assert!(addons.join("Mine").exists());
    assert!(engine.lockfile(&client).unwrap().addons.is_empty());

    // Back again with the source gone: it must come from the stash, not the network.
    server.reset().await;
    assert_eq!(
        engine.plan_switch(&client, "Raid").unwrap().restore,
        ["Demo"]
    );
    let back = engine.switch_profile(&client, "Raid").await.unwrap();
    assert_eq!(
        (back.restored.as_slice(), back.failed.len()),
        (&["Demo".to_owned()][..], 0)
    );
    assert_eq!(installed_version(&addons), "v1");

    // A friend's code for a profile nobody has stashed installs from the source.
    let code = raid.to_code(&client.flavor);
    engine.switch_profile(&client, "Bare").await.unwrap();
    let root = addons.parent().unwrap().join(".kodo-stash");
    std::fs::remove_dir_all(&root).unwrap();
    serve_release(&server, "v2").await;
    let (shared, _) = kodo_core::profile::Profile::from_code(&code).unwrap();
    let stored = engine.add_profile(&client, shared).await.unwrap();
    assert_eq!(stored.name, "Raid (2)");
    assert_eq!(
        engine.plan_switch(&client, &stored.name).unwrap().download,
        ["Demo"]
    );
    let fresh = engine.switch_profile(&client, &stored.name).await.unwrap();
    assert_eq!(fresh.installed, ["Demo"]);
    assert_eq!(installed_version(&addons), "v2");
}
