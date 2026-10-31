//! Everything between the engine and a front end: [`runtime::Runtime`] runs
//! one building and its BACnet/IP server, [`cli`] is the terminal front end.
//! The desktop app and `basidekick-sim-cli` both build on it.

pub mod cli;
pub mod runtime;

use std::path::{Path, PathBuf};

/// Where people drop their own building packages: `BASK_SIM_PACKAGES`, or
/// the platform's app-data folder.
pub fn user_packages_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BASK_SIM_PACKAGES") {
        return dir.into();
    }
    let base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    if cfg!(any(target_os = "macos", windows)) {
        base.join("BASidekick")
            .join("Building Sim")
            .join("packages")
    } else {
        base.join("basidekick-sim").join("packages")
    }
}

/// Opens a folder in Finder, Explorer or the desktop's file manager,
/// creating it first.
pub fn open_folder(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(dir)
        .spawn()
        .map(|_| ())
}
