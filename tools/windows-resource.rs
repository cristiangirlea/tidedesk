//! Shared Windows icon and version metadata for the host and viewer.

pub fn embed() {
    println!("cargo::rerun-if-env-changed=TIDEDESK_VERSION");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo::rerun-if-changed=../../assets/tidedesk.ico");
    println!("cargo::rerun-if-changed=../../assets/tidedesk.rc");
    println!("cargo::rerun-if-changed=../../tools/windows-resource.rs");

    let package_version = std::env::var("CARGO_PKG_VERSION").expect("package version");
    let version = std::env::var("TIDEDESK_VERSION").unwrap_or(package_version);
    assert!(
        !version.is_empty()
            && version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c)),
        "invalid resource version"
    );
    let numbers: Vec<u16> = version
        .split(['-', '+'])
        .next()
        .unwrap()
        .split('.')
        .map(|n| n.parse().expect("version component must fit in u16"))
        .collect();
    assert_eq!(numbers.len(), 3, "expected major.minor.patch version");
    let binary = std::env::var("CARGO_PKG_NAME").expect("package name");
    let definitions = [
        format!(
            "TIDEDESK_VERSION_NUM={},{},{},0",
            numbers[0], numbers[1], numbers[2]
        ),
        format!("TIDEDESK_VERSION=\"{version}\\0\""),
        format!("TIDEDESK_BINARY=\"{binary}\\0\""),
        format!("TIDEDESK_FILENAME=\"{binary}.exe\\0\""),
        // Task Manager, firewall and UAC prompts show this as the program's name.
        "TIDEDESK_DESCRIPTION=\"TideDesk\\0\"".to_string(),
    ];
    embed_resource::compile("../../assets/tidedesk.rc", &definitions)
        .manifest_optional()
        .expect("embedding Windows icon and version metadata");
}
