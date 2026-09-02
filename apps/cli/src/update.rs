//! Self-update from GitHub Releases (docs/cli.md § Command surface): pick the
//! newest `cli-v*` release that carries this platform's archive, download it,
//! verify it against SHA256SUMS, atomically replace the running binary.
//! Refuses inside a package manager's tree — two owners for one binary is how
//! installs rot.
//!
//! Never `releases/latest`: that is the product's `vX.Y.Z` line
//! (docs/deployment.md § CLI release), and a CLI release's assets land minutes
//! after it is published — a release with no archive yet is skipped in favour
//! of the previous one.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO: &str = "yufeng-kano/kano-proxy";

pub fn target_triple() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-musl"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-musl"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported"
    }
}

/// A binary living under a package manager's tree belongs to that manager.
pub fn package_manager_owner(exe: &Path) -> Option<&'static str> {
    let path = exe.to_string_lossy().to_lowercase();
    if path.contains("/cellar/") || path.contains("/homebrew/") || path.contains("/linuxbrew/") {
        Some("brew upgrade kano-proxy")
    } else if path.contains("\\scoop\\apps\\") || path.contains("/scoop/apps/") {
        Some("scoop update kano-proxy")
    } else {
        None
    }
}

const TAG_PREFIX: &str = "cli-v";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// `MAJOR.MINOR.PATCH` as numbers, so `1.10.0` sorts above `1.9.0`.
pub fn parse_version(v: &str) -> Option<[u64; 3]> {
    let mut it = v.trim().split('.').map(|p| p.parse::<u64>().ok());
    let out = [it.next()??, it.next()??, it.next()??];
    it.next().is_none().then_some(out)
}

/// Archive name for one CLI version on the running platform.
fn asset_name(version: &str) -> String {
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    format!("kano-proxy-{version}-{}.{ext}", target_triple())
}

/// Newest `cli-v*` release (GitHub lists newest first) that is published and
/// carries both this platform's archive and SHA256SUMS; returns its version.
fn pick_release(releases: &[Release]) -> Option<(&Release, String)> {
    releases.iter().find_map(|r| {
        if r.draft || r.prerelease {
            return None;
        }
        let version = r.tag_name.strip_prefix(TAG_PREFIX)?;
        parse_version(version)?;
        let name = asset_name(version);
        let has = |n: &str| r.assets.iter().any(|a| a.name == n);
        (has(&name) && has("SHA256SUMS")).then(|| (r, version.to_string()))
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `SHA256SUMS` line for one file name, `sha256sum` format (`<hex>  <name>`).
pub fn expected_sum<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let file = parts.next()?;
        (file.trim_start_matches('*') == name).then_some(hash)
    })
}

pub async fn cmd_update() -> Result<()> {
    let exe = std::env::current_exe().context("cannot locate the running binary")?;
    if let Some(command) = package_manager_owner(&exe) {
        bail!("this install is managed by a package manager — run `{command}` instead");
    }
    let triple = target_triple();
    if triple == "unsupported" {
        bail!("no release asset exists for this platform");
    }

    let client = reqwest::Client::builder()
        .user_agent(concat!("kano-proxy-cli/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let releases: Vec<Release> = client
        .get(format!("https://api.github.com/repos/{REPO}/releases?per_page=30"))
        .send()
        .await?
        .error_for_status()
        .context("could not list releases")?
        .json()
        .await?;

    let (release, version) = pick_release(&releases)
        .ok_or_else(|| anyhow!("no CLI release has an asset for {triple} yet"))?;
    let current = env!("CARGO_PKG_VERSION");
    if parse_version(&version) <= parse_version(current) {
        eprintln!("already up to date (v{current}; newest release is v{version})");
        return Ok(());
    }

    let asset_name = asset_name(&version);
    // Both guaranteed present by pick_release.
    let asset = release.assets.iter().find(|a| a.name == asset_name).unwrap();
    let sums_asset = release.assets.iter().find(|a| a.name == "SHA256SUMS").unwrap();

    eprintln!("downloading {asset_name}…");
    let archive = client
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let sums = client
        .get(&sums_asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let expected = expected_sum(&sums, &asset_name)
        .ok_or_else(|| anyhow!("SHA256SUMS has no entry for {asset_name}"))?;
    let actual = sha256_hex(&archive);
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("checksum mismatch for {asset_name} — refusing to install");
    }

    let binary = extract_binary(&archive, if cfg!(windows) { "zip" } else { "tar.gz" })?;
    replace_self(&exe, &binary)?;
    eprintln!("✓ updated to v{version}");
    Ok(())
}

fn extract_binary(archive: &[u8], ext: &str) -> Result<Vec<u8>> {
    let wanted = if cfg!(windows) { "kano-proxy.exe" } else { "kano-proxy" };
    if ext == "zip" {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.name().ends_with(wanted) {
                let mut out = Vec::new();
                entry.read_to_end(&mut out)?;
                return Ok(out);
            }
        }
    } else {
        let gz = flate2::read::GzDecoder::new(archive);
        let mut tar = tar::Archive::new(gz);
        for entry in tar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_path_buf();
            if path.file_name().map(|n| n == wanted).unwrap_or(false) {
                let mut out = Vec::new();
                entry.read_to_end(&mut out)?;
                return Ok(out);
            }
        }
    }
    bail!("archive does not contain {wanted}")
}

/// Rename-based swap: works on Windows too, where a running exe can be
/// renamed but not overwritten.
fn replace_self(exe: &Path, binary: &[u8]) -> Result<()> {
    let staged: PathBuf = exe.with_extension("new");
    std::fs::write(&staged, binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).context("could not move the running binary aside")?;
    if let Err(e) = std::fs::rename(&staged, exe) {
        // Roll back so the install is never left without a binary.
        let _ = std::fs::rename(&old, exe);
        return Err(e).context("could not move the new binary into place");
    }
    let _ = std::fs::remove_file(&old);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sha256sums_lines() {
        let sums = "abc123  kano-proxy-4.4.0-aarch64-apple-darwin.tar.gz\n\
                    def456 *kano-proxy-4.4.0-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(expected_sum(sums, "kano-proxy-4.4.0-aarch64-apple-darwin.tar.gz"), Some("abc123"));
        assert_eq!(expected_sum(sums, "kano-proxy-4.4.0-x86_64-pc-windows-msvc.zip"), Some("def456"));
        assert_eq!(expected_sum(sums, "missing.tar.gz"), None);
    }

    #[test]
    fn recognizes_package_manager_paths() {
        assert!(package_manager_owner(Path::new("/opt/homebrew/Cellar/kano-proxy/4.4.0/bin/kano-proxy")).is_some());
        assert!(package_manager_owner(Path::new("C:\\Users\\u\\scoop\\apps\\kano-proxy\\current\\kano-proxy.exe")).is_some());
        assert!(package_manager_owner(Path::new("/usr/local/bin/kano-proxy")).is_none());
    }

    #[test]
    fn target_triple_is_known() {
        assert_ne!(target_triple(), "unsupported");
    }

    #[test]
    fn parses_versions_numerically() {
        assert_eq!(parse_version("1.10.0"), Some([1, 10, 0]));
        assert!(parse_version("1.10.0") > parse_version("1.9.9"));
        assert_eq!(parse_version("v1.0.0"), None);
        assert_eq!(parse_version("1.0"), None);
        assert_eq!(parse_version("1.0.0.1"), None);
    }

    fn release(tag: &str, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.to_string(),
            draft: false,
            prerelease: false,
            assets: assets
                .iter()
                .map(|n| Asset { name: n.to_string(), browser_download_url: format!("https://x/{n}") })
                .collect(),
        }
    }

    #[test]
    fn picks_newest_cli_release_with_this_platforms_assets() {
        let ready = asset_name("1.1.0");
        let releases = vec![
            // Product release: wrong prefix, skipped even with assets.
            release("v4.6.0", &[&asset_name("4.6.0"), "SHA256SUMS"]),
            // Newer CLI release still building (or failed): no archive yet.
            release("cli-v1.2.0", &[]),
            // Archive present but SHA256SUMS missing: not installable.
            release("cli-v1.1.5", &[&asset_name("1.1.5")]),
            release("cli-v1.1.0", &[&ready, "SHA256SUMS"]),
            release("cli-v1.0.0", &[&asset_name("1.0.0"), "SHA256SUMS"]),
        ];
        let (picked, version) = pick_release(&releases).unwrap();
        assert_eq!(picked.tag_name, "cli-v1.1.0");
        assert_eq!(version, "1.1.0");
    }

    #[test]
    fn skips_prereleases_and_drafts() {
        let mut pre = release("cli-v2.0.0", &[&asset_name("2.0.0"), "SHA256SUMS"]);
        pre.prerelease = true;
        let mut draft = release("cli-v1.9.0", &[&asset_name("1.9.0"), "SHA256SUMS"]);
        draft.draft = true;
        let releases = vec![pre, draft, release("cli-v1.1.0", &[&asset_name("1.1.0"), "SHA256SUMS"])];
        assert_eq!(pick_release(&releases).unwrap().1, "1.1.0");
        assert!(pick_release(&[release("v4.6.0", &[])]).is_none());
    }
}
