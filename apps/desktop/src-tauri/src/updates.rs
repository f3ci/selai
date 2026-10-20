//! Public release discovery works before vault unlock. Only URLs constructed
//! here for this repository can be opened; the webview never supplies a URL.
use std::time::{Duration, Instant};

use reqwest::{Client, Url};
use selai_domain::ErrorCode;
use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::Mutex;

use crate::dto::CommandError;

const RELEASES_API: &str = "https://api.github.com/repos/f3ci/selai/releases?per_page=100";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
pub struct UpdateService {
    cached: Mutex<Option<(Instant, UpdateCheck)>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    current_version: String,
    update: Option<ReleaseUpdate>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseUpdate {
    version: String,
    notes: String,
    published_at: Option<String>,
    release_url: String,
    download: Option<Download>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Download {
    name: String,
    size: u64,
    url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    body: Option<String>,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    size: u64,
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateTarget {
    Release,
    Download,
}

fn error(message: &str) -> CommandError {
    CommandError::new(ErrorCode::Internal, message)
}

#[tauri::command]
pub async fn update_check(state: State<'_, UpdateService>) -> Result<UpdateCheck, CommandError> {
    // Deduplicate simultaneous checks and avoid repeated GitHub requests when
    // the UI remounts. Errors are never cached, so a manual retry can recover.
    let mut cached = state.cached.lock().await;
    if let Some((at, check)) = cached.as_ref() {
        if at.elapsed() < Duration::from_secs(60) {
            return Ok(check.clone());
        }
    }
    let client = Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("Selai/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| error("Could not start the update check."))?;
    let releases = fetch_releases(&client, RELEASES_API).await?;
    let check = select_update(
        releases,
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )?;
    *cached = Some((Instant::now(), check.clone()));
    Ok(check)
}

async fn fetch_releases(client: &Client, endpoint: &str) -> Result<Vec<Release>, CommandError> {
    let mut response = client
        .get(endpoint)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|_| error("Could not reach GitHub. Check your connection and try again."))?;
    if response.status() == 403 || response.status() == 429 {
        return Err(error(
            "GitHub is limiting update checks. Please try again later.",
        ));
    }
    if !response.status().is_success() {
        return Err(error(
            "GitHub releases are unavailable. Please try again later.",
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error("The update check was interrupted. Try again."))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(error("The release response is too large."));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| error("GitHub returned an invalid release response."))
}

fn repository_url(segments: &[&str]) -> Result<String, CommandError> {
    let mut url = Url::parse("https://github.com/f3ci/selai/releases/")
        .map_err(|_| error("Invalid release address."))?;
    url.path_segments_mut()
        .map_err(|_| error("Invalid release address."))?
        .pop_if_empty()
        .extend(segments.iter().copied());
    Ok(url.into())
}

fn installer_names(version: &Version, os: &str, arch: &str) -> Vec<String> {
    let arch = match (os, arch) {
        ("windows", "x86_64") => "x64",
        ("windows", "aarch64") => "arm64",
        ("linux", "x86_64") => "amd64",
        ("linux" | "macos", "aarch64") => "aarch64",
        ("macos", "x86_64") => "x64",
        _ => return Vec::new(),
    };
    let prefix = format!("Selai_{version}_{arch}");
    match os {
        "windows" => vec![format!("{prefix}-setup.exe"), format!("{prefix}_en-US.msi")],
        "linux" => vec![format!("{prefix}.AppImage"), format!("{prefix}.deb")],
        "macos" => vec![format!("{prefix}.dmg")],
        _ => Vec::new(),
    }
}

fn select_update(
    releases: Vec<Release>,
    current: &str,
    os: &str,
    arch: &str,
) -> Result<UpdateCheck, CommandError> {
    let current_version =
        Version::parse(current).map_err(|_| error("Invalid installed version."))?;
    let latest = releases
        .into_iter()
        .filter_map(|release| {
            let version = Version::parse(
                release
                    .tag_name
                    .strip_prefix('v')
                    .unwrap_or(&release.tag_name),
            )
            .ok()?;
            // The release workflow marks all 0.x versions as prereleases. Accept
            // those numeric versions, but never alpha/beta/RC or 1.x prereleases.
            if release.draft
                || !version.pre.is_empty()
                || (release.prerelease && version.major > 0)
                || !version.cmp_precedence(&current_version).is_gt()
            {
                return None;
            }
            Some((version, release))
        })
        .max_by(|(a, _), (b, _)| a.cmp_precedence(b));
    let update = if let Some((version, release)) = latest {
        let download = installer_names(&version, os, arch)
            .iter()
            .find_map(|name| {
                release.assets.iter().find(|asset| {
                    asset.name == *name && asset.state == "uploaded" && asset.size > 0
                })
            })
            .map(|asset| {
                Ok(Download {
                    name: asset.name.clone(),
                    size: asset.size,
                    url: repository_url(&["download", &release.tag_name, &asset.name])?,
                })
            })
            .transpose()?;
        Some(ReleaseUpdate {
            version: version.to_string(),
            notes: release.body.unwrap_or_default(),
            published_at: release.published_at,
            release_url: repository_url(&["tag", &release.tag_name])?,
            download,
        })
    } else {
        None
    };
    Ok(UpdateCheck {
        current_version: current.to_owned(),
        update,
    })
}

#[tauri::command]
pub async fn update_open(
    state: State<'_, UpdateService>,
    target: UpdateTarget,
) -> Result<(), CommandError> {
    let url = {
        let cached = state.cached.lock().await;
        let update = cached
            .as_ref()
            .and_then(|(_, check)| check.update.as_ref())
            .ok_or_else(|| error("Check for updates before opening a release."))?;
        match target {
            UpdateTarget::Release => update.release_url.clone(),
            UpdateTarget::Download => update
                .download
                .as_ref()
                .ok_or_else(|| error("No installer is available for this device."))?
                .url
                .clone(),
        }
    };
    tauri::async_runtime::spawn_blocking(move || webbrowser::open(&url))
        .await
        .map_err(|_| error("Could not open your browser. Please try again."))?
        .map_err(|_| {
            error("Could not open your browser. Please check your default browser and try again.")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> Release {
        serde_json::from_value(json!({"tag_name":tag,"body":"Changes","published_at":"2026-10-20T00:00:00Z",
            "assets":[{"name":format!("Selai_{}_x64-setup.exe",tag.trim_start_matches('v')),"size":1234,"state":"uploaded"}]})).unwrap()
    }

    #[test]
    fn chooses_highest_semver_not_publish_order_and_accepts_numeric_zero_prereleases() {
        let mut zero = release("v0.3.10");
        zero.prerelease = true;
        let mut rc = release("v0.4.0-rc.1");
        rc.prerelease = true;
        let mut draft = release("v2.0.0");
        draft.draft = true;
        let mut prerelease = release("v1.0.0");
        prerelease.prerelease = true;
        let check = select_update(
            vec![
                release("v0.3.9"),
                zero,
                rc,
                draft,
                prerelease,
                release("garbage"),
            ],
            "0.3.1",
            "windows",
            "x86_64",
        )
        .unwrap();
        let update = check.update.unwrap();
        assert_eq!(update.version, "0.3.10");
        assert_eq!(
            update.download.unwrap().url,
            "https://github.com/f3ci/selai/releases/download/v0.3.10/Selai_0.3.10_x64-setup.exe"
        );
    }

    #[test]
    fn never_offers_same_version_build_metadata_or_downgrade() {
        assert!(select_update(
            vec![release("v0.3.1+build.2"), release("v0.2.9")],
            "0.3.1",
            "windows",
            "x86_64"
        )
        .unwrap()
        .update
        .is_none());
    }

    #[test]
    fn matches_os_arch_and_prefers_windows_exe_over_msi() {
        let mut r = release("v0.3.2");
        for name in [
            "Selai_0.3.2_x64_en-US.msi",
            "Selai_0.3.2_aarch64.dmg",
            "Selai_0.3.2_amd64.AppImage",
        ] {
            r.assets.push(Asset {
                name: name.into(),
                size: 12,
                state: "uploaded".into(),
            });
        }
        let check = select_update(vec![r], "0.3.1", "macos", "aarch64").unwrap();
        assert_eq!(
            check.update.unwrap().download.unwrap().name,
            "Selai_0.3.2_aarch64.dmg"
        );
        assert_eq!(
            select_update(vec![release("v0.3.2")], "0.3.1", "windows", "x86_64")
                .unwrap()
                .update
                .unwrap()
                .download
                .unwrap()
                .name,
            "Selai_0.3.2_x64-setup.exe"
        );
    }

    #[test]
    fn missing_or_incomplete_installer_keeps_release_notes_available_without_wrong_arch_download() {
        let mut r = release("v0.3.2");
        r.assets[0].state = "new".into();
        assert!(select_update(vec![r], "0.3.1", "windows", "x86_64")
            .unwrap()
            .update
            .unwrap()
            .download
            .is_none());
        let update = select_update(vec![release("v0.3.2")], "0.3.1", "windows", "aarch64")
            .unwrap()
            .update
            .unwrap();
        assert!(update.download.is_none());
        assert_eq!(
            update.release_url,
            "https://github.com/f3ci/selai/releases/tag/v0.3.2"
        );
    }

    #[tokio::test]
    async fn handles_github_rate_limits_and_invalid_responses() {
        use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let err = fetch_releases(&Client::new(), &server.uri())
            .await
            .err()
            .unwrap();
        assert!(err.message.contains("limiting"));
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("invalid"))
            .mount(&server)
            .await;
        assert!(fetch_releases(&Client::new(), &server.uri())
            .await
            .err()
            .unwrap()
            .message
            .contains("invalid"));
        server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        assert!(fetch_releases(&Client::new(), &server.uri())
            .await
            .unwrap()
            .is_empty());
    }
}
