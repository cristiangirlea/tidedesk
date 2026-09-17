//! Embeds the application icon, and warns when NASM is missing: openh264 then silently builds without its
//! assembly and encodes roughly 3-4x slower.

#[path = "../../tools/windows-resource.rs"]
mod windows_resource;

fn nasm_available() -> bool {
    if std::env::var_os("NASM").is_some() || std::env::var_os("OPENH264_NO_ASM").is_some() {
        return true;
    }
    let exe = if cfg!(windows) { "nasm.exe" } else { "nasm" };
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(exe).is_file()))
        .unwrap_or(false)
}

fn main() {
    windows_resource::embed();
    println!("cargo::rerun-if-env-changed=PATH");
    println!("cargo::rerun-if-env-changed=NASM");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if matches!(arch.as_str(), "x86_64" | "x86") && !nasm_available() {
        println!(
            "cargo::warning=NASM not found: the H.264 encoder will be built without SIMD assembly and run ~3-4x slower. Install NASM (https://nasm.us) and run `cargo clean -p openh264-sys2` before rebuilding."
        );
    }
}
