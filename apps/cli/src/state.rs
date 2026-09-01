//! State file + refresh lock (docs/cli.md § State file).
//!
//! `~/.config/kano-proxy/state.json` on macOS/Linux, `%APPDATA%\kano-proxy\`
//! on Windows, mode 0600, with a sibling `state.lock` that serializes token
//! refreshes across this machine's kano-proxy processes. The file must stay
//! writable at runtime — refresh rotation persists a new token on every use.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderState {
    pub id: String,
    pub slug: String,
    /// "openai" | "anthropic"
    pub format: String,
    /// Local target base, e.g. `http://localhost:11434/v1` — includes `/v1`
    /// for openai format; only allowlisted suffixes are ever joined onto it.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expose: Vec<String>,
}

/// A `--no-tui` init's first phase, waiting for its `--auth-code` second phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingLogin {
    pub base_url: String,
    pub device_name: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_login: Option<PendingLogin>,
    #[serde(default)]
    pub providers: Vec<ProviderState>,
}

impl State {
    pub fn signed_in(&self) -> bool {
        self.device_id.is_some() && self.refresh_token.is_some()
    }
}

pub fn default_state_path() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        return base.join("kano-proxy").join("state.json");
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                home.join(".config")
            });
        base.join("kano-proxy").join("state.json")
    }
}

pub struct StateFile {
    pub path: PathBuf,
}

impl StateFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<State> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).with_context(|| {
                    format!("state file {} is not valid JSON", self.path.display())
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", self.path.display())),
        }
    }

    /// Atomic replace: write a sibling temp file, then rename over. The state
    /// holds the only copy of the current refresh token — a torn write here
    /// would sign the device out.
    pub fn save(&self, state: &State) -> Result<()> {
        let dir = self
            .path
            .parent()
            .context("state path has no parent directory")?;
        fs::create_dir_all(dir)?;
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            f.write_all(&serde_json::to_vec_pretty(state)?)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    /// Cross-process refresh serialization: exclusive-create a lock file
    /// beside the state, retrying briefly. A lock older than 30s is treated
    /// as orphaned (a crashed process) and broken — same tolerance the
    /// server's own single-flight locks use.
    pub fn acquire_lock(&self) -> Result<LockGuard> {
        let path = self.lock_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let started = Instant::now();
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let _ = writeln!(f, "{now}");
                    return Ok(LockGuard { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if started.elapsed() > Duration::from_secs(35) {
                        bail!("another kano-proxy process holds {}", path.display());
                    }
                    std::thread::sleep(Duration::from_millis(120));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn lock_is_stale(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age > Duration::from_secs(30))
        .unwrap_or(false)
}

pub struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_state() {
        let dir = std::env::temp_dir().join(format!("kano-proxy-test-{}", std::process::id()));
        let file = StateFile::new(dir.join("state.json"));
        let mut state = State::default();
        state.base_url = "https://proxy.example.com".into();
        state.refresh_token = Some("kpr_x".into());
        state.providers.push(ProviderState {
            id: "cliprov_1".into(),
            slug: "my-mac".into(),
            format: "openai".into(),
            target: "http://localhost:11434/v1".into(),
            target_key: None,
            expose: vec![],
        });
        file.save(&state).unwrap();
        let loaded = file.load().unwrap();
        assert_eq!(loaded.base_url, "https://proxy.example.com");
        assert_eq!(loaded.providers.len(), 1);
        assert_eq!(loaded.providers[0], state.providers[0]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_default() {
        let file = StateFile::new(PathBuf::from("/nonexistent/kano-proxy/state.json"));
        let state = file.load().unwrap();
        assert!(!state.signed_in());
    }

    #[test]
    fn lock_excludes_and_releases() {
        let dir = std::env::temp_dir().join(format!("kano-proxy-lock-{}", std::process::id()));
        let file = StateFile::new(dir.join("state.json"));
        {
            let _guard = file.acquire_lock().unwrap();
            assert!(file.lock_path().exists());
        }
        assert!(!file.lock_path().exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
