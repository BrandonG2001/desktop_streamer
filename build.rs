//! Build script: embeds the application icon and version information in the Windows
//! executable (shown by Explorer, the task bar and the installer's shortcuts).

/// Compiles the Windows resource script when targeting Windows.
///
/// # Errors
/// Never fails the build; a missing resource compiler only produces a warning.
fn main() {
    println!("cargo:rerun-if-changed=packaging/icons/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("packaging/icons/app.ico")
        .set("ProductName", "Virtual Desktop Streamer (Rust)")
        .set("FileDescription", "Virtual Desktop Streamer (Rust port)")
        .set("InternalName", "desktop_streamer")
        .set("OriginalFilename", "desktop_streamer.exe");
    if let Err(e) = res.compile() {
        println!("cargo:warning=cannot embed the Windows icon/version resource: {e}");
    }
}
