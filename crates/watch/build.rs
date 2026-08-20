//! Embeds the dedicated watcher icon into `gametrimmer-watch.exe` for
//! Explorer and shortcuts. The tray icon is configured separately at runtime.

fn main() {
    println!("cargo:rerun-if-changed=assets/gametrimmer-watch.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/gametrimmer-watch.ico");
    if let Err(err) = resource.compile() {
        println!("cargo:warning=не вдалося вбудувати іконку watcher в exe: {err}");
    }
}
