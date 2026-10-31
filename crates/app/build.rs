//! Gives the Windows executable its icon and version info.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/brand/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/brand/icon.ico")
            .set("ProductName", "BASidekick Building Sim")
            .set("FileDescription", "BASidekick Building Sim");
        if let Err(e) = res.compile() {
            // a missing resource compiler costs the icon, not the build
            println!("cargo:warning=no Windows icon embedded: {e}");
        }
    }
}
