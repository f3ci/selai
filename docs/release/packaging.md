# Installer packaging and signing

Backlog item S-050. The workflow is `.github/workflows/release.yml`. It builds
the Selai desktop installers on Linux, Windows and macOS, and uploads them as
workflow artifacts with checksums and SBOMs. On a `v*` tag push it also
publishes a GitHub release (see "How to run it"). It never creates a tag or a PR.

**Without the optional signing secrets (see below), every installer is UNSIGNED.**
Windows will show SmartScreen warnings and macOS Gatekeeper will block the app.

## What the workflow produces

| Platform | Runner | Installers | Other files |
| --- | --- | --- | --- |
| Linux | `ubuntu-latest` | `.deb`, `.AppImage` | `SHA256SUMS` |
| Windows | `windows-latest` | `.msi` (WiX), `.exe` (NSIS setup) | `SHA256SUMS` |
| macOS | `macos-latest` | `.dmg`, `.app` (zipped as `*.app.zip`) | `SHA256SUMS` |

Bundle targets:

- `apps/desktop/src-tauri/tauri.conf.json` sets `bundle.active` to `true`. All
  application icon sizes (`icon.ico`, `icon.icns`, and PNG variants) are
  generated from `apps/desktop/public/selai-mark.svg` with
  `pnpm tauri icon public/selai-mark.svg --output src-tauri/icons` from
  `apps/desktop`.
- Windows NSIS and MSI installers use the matching artwork in
  `apps/desktop/src-tauri/installer-art/`; the macOS DMG uses its branded
  background from that directory. The SVG files there are the artwork sources;
  their configured BMP and PNG outputs are included in the release bundles.
  NSIS's welcome/finish background and the WiX page banner stay neutral so the
  installer copy remains readable and does not overlap the Selai branding.
- `bundle.targets` is `"all"`. On Linux the workflow limits the build to
  `deb appimage` (`--bundles`), so `rpm` is not built. Windows and macOS use
  the default targets for their platform.

The SBOM job runs on Linux and produces one artifact named `sbom`:

- `<crate>.cdx.json`, one per Rust workspace member (10 files). Made with
  `cargo-cyclonedx` 0.5.9, CycloneDX 1.5.
- `frontend.cdx.json` for `apps/desktop`. Made with `@cyclonedx/cdxgen` 12.8.6,
  which reads `pnpm-lock.yaml`. pnpm 10 has no `pnpm sbom` command, so cdxgen
  is used instead.

Each platform job uploads one artifact: `installers-linux`, `installers-windows`
or `installers-macos`. Each contains the installers and a `SHA256SUMS` file.

## How to run it

- **Manual:** Actions tab, "Release packaging", "Run workflow", choose the branch.
- **Tag:** push a tag that matches `v*`. This also runs the attestation job and,
  after it, the `publish` job: it checks every platform's `SHA256SUMS`, then
  creates a GitHub release named after the tag with all installers, one combined
  `SHA256SUMS`, the SBOMs as `sbom-cyclonedx.tar.gz`, and
  `docs/release/RELEASE_NOTES.md` as its text. The release is published even
  when the attestation job fails (attestations on a private repository need
  GitHub Enterprise Cloud); the notes mention attestations only when it passed. `v0.*` tags and tags with a
  suffix (`v1.0.0-rc.1`) become pre-releases. The tag must match the version in
  `Cargo.toml`, `tauri.conf.json` and `package.json`, because the installer file
  names come from it.

## Where to find the output

Open the workflow run in the Actions tab. The artifacts are listed at the bottom
of the run summary: `installers-linux`, `installers-windows`,
`installers-macos`, `sbom`. Artifacts follow the repository's retention setting
(90 days by default). Download them with the browser, or with
`gh run download <run-id> -n installers-linux`.

## Verifying the checksums

Run this in the folder that holds the installers and `SHA256SUMS`:

```sh
sha256sum -c SHA256SUMS           # Linux
shasum -a 256 -c SHA256SUMS       # macOS
```

On Windows, compare the hash of each file with its line in `SHA256SUMS`:

```powershell
Get-FileHash -Algorithm SHA256 .\<installer-name>
```

## Verifying build provenance (tag builds only)

On tag builds the `attest` job creates a signed SLSA build provenance
attestation for every installer and SBOM. Check a file with the GitHub CLI:

```sh
gh attestation verify <installer-file> --repo f3ci/selai
```

The attestation shows the file came from this workflow in this repository. It
does not show that the file is signed for the operating system (see Signing).

## Signing (optional)

The workflow signs only when the secrets below exist. It never creates or
generates credentials. Secret values are not printed.

| Secret | Used for |
| --- | --- |
| `WINDOWS_CERTIFICATE` | Base64 PFX code-signing certificate. Windows `.msi` and `.exe` are signed with `signtool` (`scripts/release/sign-windows.ps1`). |
| `WINDOWS_CERTIFICATE_PASSWORD` | Password for that PFX. |
| `APPLE_CERTIFICATE` | Base64 `.p12` Developer ID Application certificate. Tauri imports it. |
| `APPLE_CERTIFICATE_PASSWORD` | Password for that `.p12`. |
| `APPLE_SIGNING_IDENTITY` | Identity name, for example `Developer ID Application: Name (TEAMID)`. |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | Apple ID, app-specific password and team ID for notarization. Tauri reads them. |

Notes:

- Windows signing applies to the installers. The binaries inside an NSIS or MSI
  package are not signed separately. Signing them would need a Tauri
  `bundle.windows.signCommand`.
- Apple variables are unset when empty. Tauri would otherwise treat an empty
  string as configured.
- Signing and notarization were not run. They are configured but unverified
  (see the table below).

## Update checks in the desktop app

Selai checks the public GitHub Releases API once when it starts, including on
the vault screen. A newer version adds an **Update available** button to the
top right. The dialog shows the installed and available versions, release notes,
and a download for the current OS and architecture. The version in the workspace
status bar opens a manual check. Network failures stay quiet until the user opens
the dialog; a failed check retries when the device comes back online.

Version comparison uses semantic versions, never release publication order.
Numeric 0.x releases are included even though this workflow marks them as
prereleases; draft and alpha/beta/RC releases are excluded. Checks use the latest
100 published releases and a 60-second in-process cache. No GitHub token or
vault data is sent. GitHub's unauthenticated API limits still apply.

**Download update** opens the matching installer in the default browser. The app
stays open; the user saves work and closes Selai before running the downloaded
installer. Missing architecture assets fall back to release details. This uses
the existing release installers and requires no additional release secrets.

## Not included

- **In-app installation.** Update discovery and browser downloads are included.
  No updater artifacts (`.sig`, `latest.json`) are produced and
  `TAURI_SIGNING_PRIVATE_KEY` is not used. Adding the updater needs a signing key
  pair and a hosting decision.
- **Automatic publication from a manual workflow run.** Manual runs produce
  workflow artifacts; a `v*` tag triggers the publish job described above.

## What is NOT verified

| Item | Status |
| --- | --- |
| Windows build (`.msi`, `.exe`) | Built successfully by the GitHub release workflow for `v0.3.1`; the native vault checks passed before upload. |
| macOS build (`.dmg`, `.app.zip`) | Built successfully by the GitHub release workflow for `v0.3.1`. |
| Linux build (`.deb`, `.AppImage`) | Built successfully by the GitHub release workflow for `v0.3.1`. |
| Windows signing (`sign-windows.ps1`) | Optional and runs only when the `WINDOWS_CERTIFICATE` secret is configured. |
| Apple signing and notarization | Optional and runs only when the Apple signing secrets are configured. |
| `signtool` path discovery on `windows-latest` | Windows release workflow completed successfully for `v0.3.1`. |
| `-- --locked` passed through to cargo by `pnpm tauri build` | Confirmed by successful release builds on all three platforms for `v0.3.1`. |
| Attestation (`attest` job) | Build provenance completed successfully for `v0.3.1`. |
| SBOM job on a clean runner | Completed successfully in the GitHub release workflow for `v0.3.1`. |
| Update discovery and download | Covered by Rust and frontend tests. Installation remains a manual installer flow. |
| Icons | `icon.png`, `icon.ico`, `icon.icns`, and platform sizes are present. The Windows and macOS bundles have not been run here. |

## Helper scripts

- `scripts/release/collect-bundles.sh <bundle-dir> <stage-dir>`: Linux and macOS.
  Copies installers, zips the `.app`, writes and checks `SHA256SUMS`.
- `scripts/release/collect-bundles.ps1`: the Windows version of the above.
- `scripts/release/sign-windows.ps1`: Windows signing, only when secrets are set.
- `scripts/release/generate-sbom.sh <out-dir>`: both SBOM kinds.

## Tool and action choices

- Checkout, pnpm, Node, Rust cache and toolchain steps match `ci.yml`
  (`checkout@v4`, `pnpm/action-setup@v4` with pnpm 10, `setup-node@v4` with
  Node 22, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`).
- `actions/upload-artifact@v7` and `actions/download-artifact@v8`: current major
  tags, checked with `git ls-remote --tags`.
- `actions/attest-build-provenance@v4`: current major tag, as requested. Its README
  says v4 is a wrapper and recommends `actions/attest` for new workflows. If that
  is preferred, change the step to `actions/attest` (same `subject-path` input).
- Attestation permissions are on the `attest` job only: `id-token: write`,
  `attestations: write`, `artifact-metadata: write`. The top-level permission is
  `contents: read`.
