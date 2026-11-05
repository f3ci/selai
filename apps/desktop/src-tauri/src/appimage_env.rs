//! Keep the AppImage's private library directory out of host child processes.
//!
//! AppRun sets `LD_LIBRARY_PATH` so Selai can load its bundled libraries. A
//! local shell must use the host's matching system libraries instead, or a
//! bundled `libcrypto.so.3` can be paired with an incompatible host `libssl`.

use std::{env, ffi::OsString, path::Path};

#[cfg(target_os = "linux")]
use portable_pty::CommandBuilder;
#[cfg(target_os = "linux")]
use tokio::process::Command;

fn override_value(appdir: &Path, current: &std::ffi::OsStr) -> Option<Option<OsString>> {
    let paths: Vec<_> = env::split_paths(current)
        .filter(|path| !path.starts_with(appdir))
        .collect();
    let filtered = if paths.is_empty() {
        None
    } else {
        env::join_paths(paths).ok()
    };
    if filtered.as_deref() == Some(current) {
        None
    } else {
        Some(filtered)
    }
}

#[cfg(target_os = "linux")]
fn current_override() -> Option<Option<OsString>> {
    let appdir = env::var_os("APPDIR")?;
    let library_path = env::var_os("LD_LIBRARY_PATH")?;
    override_value(Path::new(&appdir), &library_path)
}

#[cfg(target_os = "linux")]
pub(crate) fn sanitize_tokio_command(command: &mut Command) {
    if let Some(path) = current_override() {
        match path {
            Some(path) => {
                command.env("LD_LIBRARY_PATH", path);
            }
            None => {
                command.env_remove("LD_LIBRARY_PATH");
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn sanitize_pty_command(command: &mut CommandBuilder) {
    if let Some(path) = current_override() {
        match path {
            Some(path) => {
                command.env("LD_LIBRARY_PATH", path);
            }
            None => {
                command.env_remove("LD_LIBRARY_PATH");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_appimage_library_paths_and_keeps_host_paths() {
        let appdir = Path::new("/tmp/.mount_selai123");
        let input = env::join_paths([
            appdir.join("usr/lib"),
            Path::new("/usr/local/lib").to_path_buf(),
            appdir.join("usr/lib/x86_64-linux-gnu"),
            Path::new("/opt/company/lib").to_path_buf(),
        ])
        .unwrap();

        let filtered = override_value(appdir, &input).unwrap().unwrap();
        assert_eq!(
            env::split_paths(&filtered).collect::<Vec<_>>(),
            [
                Path::new("/usr/local/lib").to_path_buf(),
                Path::new("/opt/company/lib").to_path_buf()
            ]
        );
    }

    #[test]
    fn removes_library_path_when_it_only_contains_appimage_paths() {
        let appdir = Path::new("/tmp/.mount_selai123");
        let input = env::join_paths([appdir.join("usr/lib")]).unwrap();

        assert_eq!(override_value(appdir, &input), Some(None));
    }

    #[test]
    fn leaves_library_path_unchanged_outside_an_appimage() {
        let appdir = Path::new("/tmp/.mount_selai123");
        let input = env::join_paths([
            Path::new("/usr/local/lib").to_path_buf(),
            Path::new("/opt/lib").to_path_buf(),
        ])
        .unwrap();

        assert_eq!(override_value(appdir, &input), None);
    }
}
