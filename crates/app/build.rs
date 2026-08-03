//! Embeds the application icon and a DPI-awareness manifest into the
//! Windows executable resource. The icon makes Explorer, the taskbar and
//! shortcuts show it (the in-window icon is set separately at runtime, see
//! `main.rs` - winit does not read the resource). The manifest declares
//! `PerMonitorV2` DPI awareness explicitly.
//!
//! `winit` (via `eframe`) already calls `SetProcessDpiAwarenessContext` at
//! startup on its own, so this manifest is defense-in-depth rather than a
//! fix for an observed bug - it does not replace the manual 100/150/200%
//! scaling checks in `docs/portability-test-cases.md`. It also sets
//! `longPathAware`, which lifts the MAX_PATH=260 limit for this process on
//! deeply nested portable install locations.

fn main() {
    println!("cargo:rerun-if-changed=assets/gametrimmer.ico");
    println!("cargo:rerun-if-changed=assets/gametrimmer.manifest");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/gametrimmer.ico");
    resource.set_manifest_file("assets/gametrimmer.manifest");
    if let Err(err) = resource.compile() {
        // A missing rc.exe/windres must not brick the build - the app is
        // fully functional without the embedded icon/manifest (winit's
        // runtime DPI call still applies), so degrade to a visible warning
        // instead.
        println!("cargo:warning=не вдалося вбудувати іконку/маніфест в exe: {err}");
    }
}
