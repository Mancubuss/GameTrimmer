//! Embeds the application icon into the Windows executable resource, so
//! Explorer, the taskbar and shortcuts show it. The in-window icon is set
//! separately at runtime (see `main.rs`) - winit does not read the resource.

fn main() {
    println!("cargo:rerun-if-changed=assets/gametrimmer.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/gametrimmer.ico");
    if let Err(err) = resource.compile() {
        // A missing rc.exe/windres must not brick the build - the app is
        // fully functional without the embedded icon, so degrade to a
        // visible warning instead.
        println!("cargo:warning=не вдалося вбудувати іконку в exe: {err}");
    }
}
