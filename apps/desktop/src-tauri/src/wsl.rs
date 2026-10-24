//! WSL discovery never starts a distribution. Shells use argv, not interpolation.

use crate::dto::CommandError;
use selai_domain::ErrorCode;

pub(crate) fn executable() -> std::path::PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| {
            std::path::PathBuf::from(root)
                .join("System32")
                .join("wsl.exe")
        })
        .unwrap_or_else(|| "wsl.exe".into())
}

pub(crate) fn validate(distribution: &str) -> Result<(), CommandError> {
    if distribution.is_empty()
        || distribution.len() > 256
        || distribution.starts_with('-')
        || distribution.chars().any(char::is_control)
        || distribution.contains('|')
    {
        return Err(CommandError::new(
            ErrorCode::InvalidInput,
            "invalid WSL distribution name",
        ));
    }
    if !cfg!(windows) {
        return Err(CommandError::new(
            ErrorCode::InvalidInput,
            "WSL targets are only available on Windows",
        ));
    }
    Ok(())
}

/// wsl.exe normally writes UTF-16LE, including when redirected to a pipe.
#[cfg(any(windows, test))]
fn parse_distributions(bytes: &[u8]) -> Vec<String> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.contains(&0) {
        let words: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&words)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    let mut names: Vec<String> = text
        .trim_start_matches('\u{feff}')
        .trim_end_matches('\0')
        .lines()
        .map(str::trim)
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 256
                && !name.starts_with('-')
                && !name.chars().any(char::is_control)
                && !name.contains('|')
        })
        .map(str::to_owned)
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names
}

pub(crate) async fn distributions() -> Result<Vec<String>, CommandError> {
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
    #[cfg(windows)]
    {
        use std::time::Duration;
        let mut command = tokio::process::Command::new(executable());
        command
            .args(["--list", "--quiet"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(10), command.output()).await {
            Ok(Ok(output)) if output.status.success() => Ok(parse_distributions(&output.stdout)),
            Ok(Ok(_)) => Ok(Vec::new()), // WSL is disabled or has no distributions.
            Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            _ => Err(CommandError::new(
                ErrorCode::Internal,
                "Could not detect WSL distributions. Refresh hosts to retry.",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_windows_utf16_output_with_and_without_bom() {
        for text in [
            "Debian\r\nUbuntu-24.04\r\n",
            "\u{feff}Debian\r\nUbuntu-24.04\r\n",
        ] {
            let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            assert_eq!(parse_distributions(&bytes), vec!["Debian", "Ubuntu-24.04"]);
        }
    }

    #[test]
    fn accepts_utf8_deduplicates_and_ignores_unsafe_names() {
        assert_eq!(
            parse_distributions(b"Ubuntu\nubuntu\n\n--help\ninvalid|name\nDebian\n"),
            vec!["Debian", "Ubuntu"]
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn non_windows_lists_only_the_native_local_target() {
        let services = crate::service::Services::in_memory(std::sync::Arc::new(
            selai_secrets::MemoryStore::new(),
        ))
        .expect("services");
        let hosts = services.local_hosts().await.expect("local targets");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].kind, "local");
        assert!(hosts[0].wsl_distribution.is_none());
    }
}
