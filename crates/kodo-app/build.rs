//! Embed the app icon into kodo-app.exe; nothing to do on other targets.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=assets/kodo.rc");
    println!("cargo::rerun-if-changed=assets/icon/kodo.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/kodo.rc", embed_resource::NONE).manifest_required()?;
    }
    Ok(())
}
