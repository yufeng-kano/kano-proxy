//! Command implementations (docs/cli.md § Command surface). The TUI screens
//! and the `--no-tui` flags feed the same code paths.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::api;
use crate::state::{PendingLogin, ProviderState, State, StateFile};
use crate::tui;
use crate::tunnel::{self, TokenCache};

pub const EXIT_AUTH_REJECTED: i32 = 2;

/// Every state write goes through here: re-read and save while holding the
/// same lock token refreshes use, so a concurrent command (or its refresh
/// rotation) can never be clobbered by a stale snapshot — the failure mode
/// that resurrects a superseded refresh token and trips the server's
/// reuse-as-theft revocation.
fn mutate_state(file: &StateFile, f: impl FnOnce(&mut State)) -> Result<()> {
    let _lock = file.acquire_lock()?;
    let mut state = file.load()?;
    f(&mut state);
    file.save(&state)
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this-machine".to_string())
}

/// Hostname → valid slug (lowercase 2–32, alnum + inner hyphens), uniquified
/// against the state's existing providers.
pub fn default_slug(existing: &[ProviderState]) -> String {
    let raw = hostname().to_lowercase();
    let mut slug: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    slug.truncate(32);
    let slug = slug.trim_matches('-').to_string();
    let base = if slug.len() < 2 { "local".to_string() } else { slug };
    if !existing.iter().any(|p| p.slug == base) {
        return base;
    }
    for n in 2..100 {
        let candidate = format!("{}-{n}", &base[..base.len().min(29)]);
        if !existing.iter().any(|p| p.slug == candidate) {
            return candidate;
        }
    }
    base
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(url).spawn().is_ok();
    #[cfg(target_os = "windows")]
    let opened = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn().is_ok();
    #[cfg(all(unix, not(target_os = "macos")))]
    let opened = std::process::Command::new("xdg-open").arg(url).spawn().is_ok();
    if opened {
        eprintln!("→ opening {url}");
    }
}

// ---------------------------------------------------------------------------
// init

pub struct InitArgs {
    pub no_tui: bool,
    pub base_url: Option<String>,
    pub device_name: Option<String>,
    pub auth_code: Option<String>,
}

pub async fn cmd_init(file: &StateFile, args: InitArgs) -> Result<()> {
    let mut state = file.load()?;

    // Phase 2 of a --no-tui init: redeem the code against the pending request.
    if let Some(code) = &args.auth_code {
        let pending = state
            .pending_login
            .clone()
            .context("no pending sign-in — run `kano-proxy init --no-tui --base-url … --device-name …` first")?;
        let done = api::login_complete(&pending.base_url, &pending.request_id, code).await?;
        state.base_url = pending.base_url.clone();
        state.device_id = Some(done.device_id);
        state.device_name = Some(pending.device_name.clone());
        state.refresh_token = Some(done.refresh_token);
        state.pending_login = None;
        file.save(&state)?;
        eprintln!("✓ device \"{}\" signed in", pending.device_name);
        return Ok(());
    }

    // Re-running on an initialized state file refuses with a hint — revoke and
    // re-init deliberately, not by accident (docs/cli.md).
    if state.signed_in() {
        bail!(
            "this device is already signed in as \"{}\" against {} — revoke it on the web UI's CLI page (or delete {}) before running init again",
            state.device_name.as_deref().unwrap_or("?"),
            state.base_url,
            file.path.display()
        );
    }

    if args.no_tui {
        let base = api::normalize_base_url(
            &args.base_url.context("--base-url is required with --no-tui")?,
        )?;
        let device_name = args.device_name.context("--device-name is required with --no-tui")?;
        let start = api::login_start(&base, &device_name).await?;
        state.pending_login = Some(PendingLogin {
            base_url: base,
            device_name,
            request_id: start.request_id,
        });
        file.save(&state)?;
        println!("open {}  then run:", start.verify_url);
        println!("kano-proxy init --no-tui --auth-code XXXX-XXXX");
        return Ok(());
    }

    tui::require_tty()?;
    let base = api::normalize_base_url(&tui::input(
        "kano-proxy init — sign this device in",
        "Server",
        args.base_url.as_deref().unwrap_or(&state.base_url),
    )?)?;
    let device_name = tui::input(
        "kano-proxy init — sign this device in",
        "Device name",
        args.device_name.as_deref().unwrap_or(&hostname()),
    )?;
    let start = api::login_start(&base, &device_name).await?;
    // Always printed, for SSH — the browser may be on another machine.
    eprintln!("→ approve this device at: {}", start.verify_url);
    open_browser(&start.verify_url);
    let code = tui::input("kano-proxy init — sign this device in", "Code shown in browser", "")?;
    let done = api::login_complete(&base, &start.request_id, &code).await?;
    state.base_url = base;
    state.device_id = Some(done.device_id);
    state.device_name = Some(device_name.clone());
    state.refresh_token = Some(done.refresh_token);
    state.pending_login = None;
    file.save(&state)?;
    eprintln!("✓ device \"{device_name}\" signed in");
    Ok(())
}

// ---------------------------------------------------------------------------
// add

pub struct AddArgs {
    pub no_tui: bool,
    pub slug: Option<String>,
    pub format: Option<String>,
    pub target: Option<String>,
    pub target_key: Option<String>,
    pub expose: Option<String>,
}

fn parse_format(raw: &str) -> Result<String> {
    match raw {
        "openai" | "anthropic" => Ok(raw.to_string()),
        _ => bail!("format must be 'openai' or 'anthropic'"),
    }
}

fn normalize_target(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .context("target must start with http:// or https:// (e.g. http://localhost:11434/v1)")?;
    // A scheme with no host ("http://", "http:///v1") would register a
    // provider whose every tunneled request fails at send time.
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        bail!("target is missing a host (e.g. http://localhost:11434/v1)");
    }
    Ok(trimmed.to_string())
}

pub async fn cmd_add(file: &StateFile, args: AddArgs) -> Result<()> {
    let state = file.load()?;
    api::require_signed_in(&state)?;

    let (slug, format, target, target_key, expose, initial_models) = if args.no_tui {
        let slug = args.slug.context("--slug is required with --no-tui")?;
        let format = parse_format(&args.format.context("--format is required with --no-tui")?)?;
        let target = normalize_target(&args.target.context("--target is required with --no-tui")?)?;
        let expose: Vec<String> = args
            .expose
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // Models are probed on the first `start` (docs/cli.md).
        (slug, format, target, args.target_key, expose, Vec::new())
    } else {
        tui::require_tty()?;
        let title = "kano-proxy add — register a local endpoint";
        let slug = tui::input(title, "Slug", &default_slug(&state.providers))?;
        let format = match tui::choose(title, &["openai", "anthropic"])? {
            1 => "anthropic".to_string(),
            _ => "openai".to_string(),
        };
        let default_target = if format == "anthropic" { "http://localhost:11434" } else { "http://localhost:11434/v1" };
        let target = normalize_target(&tui::input(title, "Target base URL (include /v1 for openai)", default_target)?)?;
        let key = tui::input_secret(title, "Local API key (Enter for none)")?;
        let target_key = if key.is_empty() { None } else { Some(key) };

        match api::probe_local_models(&format, &target, target_key.as_deref()).await {
            Ok(models) if !models.is_empty() => {
                eprintln!("✓ found {} models", models.len());
                match tui::pick_models(&format!("{title} — expose which models?"), &models)? {
                    // "All models (follow local server)": store no filter, so
                    // models added locally later appear automatically.
                    None => (slug, format, target, target_key, Vec::new(), Vec::new()),
                    Some(subset) => (slug, format, target, target_key, subset, Vec::new()),
                }
            }
            _ => {
                // A target that is not running yet must not block registration:
                // ask for ids by hand; the first connect overwrites with truth.
                let manual = tui::input(
                    title,
                    "Couldn't reach the target — model ids, comma-separated (Enter for none)",
                    "-",
                )?;
                let initial: Vec<String> = if manual == "-" {
                    Vec::new()
                } else {
                    manual.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                };
                (slug, format, target, target_key, Vec::new(), initial)
            }
        }
    };

    let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
    let access = tokens.get(false).await?;
    let created = api::create_provider(&state.base_url, &access, &slug, &format, &expose, &initial_models).await?;

    mutate_state(file, |state| {
        state.providers.push(ProviderState {
            id: created.id,
            slug: created.slug.clone(),
            format,
            target,
            target_key,
            expose,
        })
    })?;
    eprintln!("✓ registered \"{}\" — run `kano-proxy start` to bring it online", created.slug);
    Ok(())
}

// ---------------------------------------------------------------------------
// remove / list / status

pub async fn cmd_remove(file: &StateFile, slug: &str, local_only: bool) -> Result<()> {
    let state = file.load()?;
    let Some(index) = state.providers.iter().position(|p| p.slug == slug) else {
        bail!("no provider \"{slug}\" in {}", file.path.display());
    };
    if !local_only {
        api::require_signed_in(&state)?;
        let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
        let access = tokens.get(false).await?;
        let id = state.providers[index].id.clone();
        api::delete_provider(&state.base_url, &access, &id).await?;
    }
    mutate_state(file, |state| state.providers.retain(|p| p.slug != slug))?;
    eprintln!("✓ removed \"{slug}\"");
    Ok(())
}

pub async fn cmd_list(file: &StateFile) -> Result<()> {
    let state = file.load()?;
    api::require_signed_in(&state)?;
    let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
    let access = tokens.get(false).await?;
    let remote = api::list_providers(&state.base_url, &access).await?;

    println!("{:<20} {:<10} {:<32} {:<11} {:>6}  {}", "SLUG", "FORMAT", "TARGET", "STATE", "MODELS", "LAST REPORT");
    for p in &state.providers {
        let item = remote.iter().find(|r| r.id == p.id);
        let (conn, count, updated) = match item {
            Some(r) => (
                if r.connected { "connected" } else { "offline" },
                r.models.len().to_string(),
                r.models_updated_at.clone().unwrap_or_else(|| "never".into()),
            ),
            None => ("deleted", "-".into(), "-".into()),
        };
        println!("{:<20} {:<10} {:<32} {:<11} {:>6}  {}", p.slug, p.format, p.target, conn, count, updated);
    }
    for r in remote.iter().filter(|r| !state.providers.iter().any(|p| p.id == r.id)) {
        println!("{:<20} {:<10} {:<32} {:<11} {:>6}  (registered on another device: {})",
            r.slug, r.format, "-", if r.connected { "connected" } else { "offline" }, r.models.len(),
            r.device_name.as_deref().unwrap_or("?"));
    }
    Ok(())
}

pub async fn cmd_status(file: &StateFile) -> Result<()> {
    let state = file.load()?;
    if !state.signed_in() {
        println!("not signed in — run `kano-proxy init`");
        return Ok(());
    }
    println!(
        "signed in as \"{}\" against {}",
        state.device_name.as_deref().unwrap_or("?"),
        state.base_url
    );
    let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
    match tokens.get(false).await {
        Ok(access) => {
            println!("token: fresh (rotated just now)");
            match api::list_providers(&state.base_url, &access).await {
                Ok(remote) => {
                    for p in &state.providers {
                        let conn = remote
                            .iter()
                            .find(|r| r.id == p.id)
                            .map(|r| if r.connected { "connected" } else { "offline" })
                            .unwrap_or("deleted on server");
                        println!("  {:<20} {}", p.slug, conn);
                    }
                }
                Err(e) => println!("  providers: unavailable ({e})"),
            }
        }
        Err(e) => {
            println!("token: rejected ({e})");
            std::process::exit(EXIT_AUTH_REJECTED);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// start

pub async fn cmd_start(file: StateFile, concurrency: usize) -> Result<()> {
    let state = file.load()?;
    api::require_signed_in(&state)?;
    if state.providers.is_empty() {
        bail!("no providers registered — run `kano-proxy add` first");
    }
    // A raise above the server's cap is refused (docs/cli.md).
    if !(1..=4).contains(&concurrency) {
        bail!("--concurrency must be between 1 and 4");
    }

    let file = Arc::new(file);
    let tokens = Arc::new(TokenCache::new(file.clone()));
    let shutdown = Arc::new(AtomicBool::new(false));

    let ctrl_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("shutting down…");
        ctrl_shutdown.store(true, Ordering::SeqCst);
    });
    #[cfg(unix)]
    {
        let term_shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                sig.recv().await;
                term_shutdown.store(true, Ordering::SeqCst);
            }
        });
    }

    let mut tasks = Vec::new();
    for provider in state.providers.clone() {
        tasks.push(tokio::spawn(tunnel::run_provider(
            file.clone(),
            tokens.clone(),
            provider,
            concurrency,
            shutdown.clone(),
        )));
    }

    let mut auth_rejected = false;
    for task in tasks {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!("{e}");
                auth_rejected = true;
            }
            Err(_) => {}
        }
    }
    if auth_rejected {
        std::process::exit(EXIT_AUTH_REJECTED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(slug: &str) -> ProviderState {
        ProviderState {
            id: format!("id-{slug}"),
            slug: slug.into(),
            format: "openai".into(),
            target: "http://localhost:11434/v1".into(),
            target_key: None,
            expose: vec![],
        }
    }

    #[test]
    fn default_slug_uniquifies() {
        let existing = vec![];
        let base = default_slug(&existing);
        assert!(base.len() >= 2);
        assert!(base.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        let taken = vec![provider(&base)];
        let next = default_slug(&taken);
        assert_ne!(next, base);
        assert!(next.ends_with("-2"));
    }

    #[test]
    fn format_and_target_validation() {
        assert!(parse_format("openai").is_ok());
        assert!(parse_format("anthropic").is_ok());
        assert!(parse_format("grpc").is_err());
        assert_eq!(normalize_target("http://localhost:11434/v1/").unwrap(), "http://localhost:11434/v1");
        assert!(normalize_target("localhost:11434").is_err());
        assert!(normalize_target("http://").is_err());
        assert!(normalize_target("http:///v1").is_err());
    }
}
