//! Embeds the application icon and version into the Windows executable.

#[path = "../../tools/windows-resource.rs"]
mod windows_resource;

fn main() {
    windows_resource::embed();
}
