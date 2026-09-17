//! Embeds the application icon into the Windows executable.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo::rerun-if-changed=../../assets/tidedesk.ico");
        embed_resource::compile("../../assets/tidedesk.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("embedding the application icon");
    }
}
