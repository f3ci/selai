// Hide the extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = selai_desktop_lib::ssh_auth::helper_main() {
        std::process::exit(code);
    }
    selai_desktop_lib::run();
}
