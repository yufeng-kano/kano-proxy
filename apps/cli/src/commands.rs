//! Command implementations (docs/cli.md § Command surface). The TUI screens
//! and the `--no-tui` flags feed the same code paths.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::agents::{self, AgentKind, RestoreArgs};
use crate::api;
use crate::state::{PendingLogin, ProviderState, State, StateFile};
use crate::tui::{self, Nav};
use crate::tunnel::{self, TokenCache};

pub const EXIT_AUTH_REJECTED: i32 = 2;

fn mutate_state(file: &StateFile, f: impl FnOnce(&mut State)) -> Result<()> {
    file.mutate(f)
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

/// The hosted edition — what the Server prompt offers when this machine has
/// no state file yet (docs/cli.md § Command surface, operator decision
/// 2026-09-22). Self-hosters type their own origin over it, or pass
/// `--base-url`; the web UI's install card shows each instance's own.
pub const DEFAULT_BASE_URL: &str = "https://kano-proxy.yuufeng.com";

/// What the Server prompt is pre-filled with: an explicit flag, else the
/// origin this state file already knows (a `--no-tui` first phase, or a file
/// whose credentials were revoked and cleared), else the hosted edition.
pub fn server_prompt_default<'a>(flag: Option<&'a str>, state_base_url: &'a str) -> &'a str {
    match flag {
        Some(f) if !f.trim().is_empty() => f,
        _ if !state_base_url.trim().is_empty() => state_base_url,
        _ => DEFAULT_BASE_URL,
    }
}

/// Best-effort browser launch. Says it opened one only when that is
/// plausible: on Linux/BSD a spawned `xdg-open` with no display session
/// "succeeds" and opens nothing, which over SSH misleads the user into
/// waiting for a page that never appears — the URL is printed regardless.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(url).spawn().is_ok();
    #[cfg(target_os = "windows")]
    let opened = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn().is_ok();
    #[cfg(all(unix, not(target_os = "macos")))]
    let opened = {
        let has_display = ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|v| std::env::var_os(v).is_some_and(|d| !d.is_empty()));
        has_display && std::process::Command::new("xdg-open").arg(url).spawn().is_ok()
    };
    if opened {
        eprintln!("→ opening it in your browser");
    }
}

// ---------------------------------------------------------------------------
// login

pub struct LoginArgs {
    pub no_tui: bool,
    pub base_url: Option<String>,
    pub device_name: Option<String>,
    pub auth_code: Option<String>,
}

pub async fn cmd_login(file: &StateFile, args: LoginArgs) -> Result<()> {
    let mut state = file.load()?;

    // Signed in locally: find out whether the server still agrees. A device the
    // server revoked (Access page, theft detection) must be able to sign in
    // again without hand-editing the state file; a device the server still
    // accepts is refused — sign out deliberately, not by accident (docs/cli.md).
    if state.signed_in() && args.auth_code.is_none() {
        let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
        match tokens.get(false).await {
            Ok(_) => bail!(
                "this device is already signed in as \"{}\" against {} — run `kano-proxy logout` first",
                state.device_name.as_deref().unwrap_or("?"),
                state.base_url,
            ),
            Err(e) if api::is_auth_rejection(&e) => {
                eprintln!(
                    "· the server no longer accepts this device (\"{}\" was revoked or signed out there) — signing in again",
                    state.device_name.as_deref().unwrap_or("?")
                );
                mutate_state(file, |s| {
                    s.device_id = None;
                    s.device_name = None;
                    s.refresh_token = None;
                    s.providers.clear();
                    s.agents.clear();
                })?;
                state = file.load()?;
            }
            Err(e) => return Err(e.context("cannot tell whether this device is still signed in — check the connection, or run `kano-proxy logout`")),
        }
    }

    // Phase 2 of a --no-tui login: redeem the code against the pending request.
    if let Some(code) = &args.auth_code {
        let pending = state
            .pending_login
            .clone()
            .context("no pending sign-in — run `kano-proxy login --no-tui --base-url … --device-name …` first")?;
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
        println!("kano-proxy login --no-tui --auth-code XXXX-XXXX");
        return Ok(());
    }

    tui::require_tty()?;
    let title = "kano-proxy login — sign this device in";
    // Three steps; Esc re-enters the previous one with its answer (tui.rs). Going
    // back from the code screen abandons that login request — the next one is new.
    let mut server = String::new();
    let mut device_name = String::new();
    let mut start: Option<api::LoginStart> = None;
    let mut step = 0usize;
    let (base, device_name, start, code) = loop {
        match step {
            0 => match tui::input_with_note(title, &[], "Server", server_prompt_default(args.base_url.as_deref(), &state.base_url), &server)? {
                Nav::Next(v) => {
                    server = api::normalize_base_url(&v)?;
                    step = 1;
                }
                Nav::Back => {}
                Nav::Exit => return Err(tui::cancelled()),
            },
            1 => match tui::input_with_note(title, &[], "Device name", args.device_name.as_deref().unwrap_or(&hostname()), &device_name)? {
                Nav::Next(v) => {
                    device_name = v;
                    start = None;
                    step = 2;
                }
                Nav::Back => step = 0,
                Nav::Exit => return Err(tui::cancelled()),
            },
            _ => {
                let request = match &start {
                    Some(s) => s,
                    None => {
                        let s = api::login_start(&server, &device_name).await?;
                        // Printed for the scrollback, and drawn on the code screen below:
                        // the screens run on the alternate screen, so this line is hidden
                        // while the code prompt is up — over SSH the browser is on another
                        // machine and the URL is the only way through (docs/cli.md).
                        eprintln!("→ approve this device at: {}", s.verify_url);
                        open_browser(&s.verify_url);
                        start.insert(s)
                    }
                };
                let note = [
                    "Sign in with Google and press Approve at:",
                    request.verify_url.as_str(),
                    "",
                    "Then paste the one-time code the page shows.",
                ];
                match tui::input_with_note(title, &note, "Code shown in browser", "", "")? {
                    Nav::Next(code) => break (server.clone(), device_name.clone(), start.take().expect("a request"), code),
                    Nav::Back => step = 1,
                    Nav::Exit => return Err(tui::cancelled()),
                }
            }
        }
    };
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

/// `tunnel add`'s answers, held for the review step: slug, format, target, local key,
/// expose whitelist, hand-typed initial models.
type AddAnswers = (String, String, String, Option<String>, Vec<String>, Vec<String>);

fn parse_format(raw: &str) -> Result<String> {
    match raw {
        "openai" | "anthropic" => Ok(raw.to_string()),
        _ => bail!("format must be 'openai' or 'anthropic'"),
    }
}

/// What the `add` screen shows for an `openai` target: the `/v1` base, so the
/// CLI's allowlisted suffixes (`/chat/completions`, `/models`) join onto it.
const OPENAI_DEFAULT_TARGET: &str = "http://localhost:11434/v1";
const OPENAI_TARGET_NOTE: [&str; 7] = [
    "Enter the server's OpenAI-compatible base URL, ending in /v1.",
    "The CLI appends /chat/completions and /models to it.",
    "",
    "  Ollama      http://localhost:11434/v1",
    "  LM Studio   http://localhost:1234/v1",
    "  vLLM        http://localhost:8000/v1",
    "  llama.cpp   http://localhost:8080/v1",
];

/// What the `add` screen shows for an `anthropic` target: the origin only —
/// the CLI appends `/v1/messages` and `/v1/models` itself.
const ANTHROPIC_DEFAULT_TARGET: &str = "http://localhost:11434";
const ANTHROPIC_TARGET_NOTE: [&str; 5] = [
    "Enter the server's origin only — no /v1 on the end.",
    "The CLI appends /v1/messages and /v1/models to it.",
    "",
    "  Ollama      http://localhost:11434",
    "  llama.cpp   http://localhost:8080",
];

fn normalize_target(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    // A real URL parse, not string inspection: shapes like "http://:11434/v1"
    // or "http://user@/v1" have a non-empty authority substring but no host,
    // and would register a provider whose every tunneled request fails.
    let url = reqwest::Url::parse(trimmed)
        .map_err(|_| anyhow::anyhow!("target must be a full http(s) URL (e.g. http://localhost:11434/v1)"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("target must start with http:// or https:// (e.g. http://localhost:11434/v1)");
    }
    if url.host_str().is_none_or(str::is_empty) {
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
        add_root(&state).await?
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
    eprintln!("✓ registered \"{}\" — run `kano-proxy tunnel start` to bring it online", created.slug);
    Ok(())
}

/// `tunnel add`'s root page (docs/cli.md § Navigation): every answer on one page,
/// each opened with Enter and applied to the draft with Enter; Tab on this page
/// registers the endpoint, Esc twice leaves — after asking, since registering
/// would create something on the server.
async fn add_root(state: &State) -> Result<AddAnswers> {
    let title = "kano-proxy tunnel add — register a local endpoint";
    // The choice names the HTTP API the local server speaks, and the target page is
    // written for that API alone: its own shape rule, its own default, and
    // copy-pasteable examples for the usual local servers — the user should never
    // have to work out whether "/v1" belongs on the end (docs/cli.md § Command surface).
    let format_note = ["Which HTTP API does the local server speak?", ""];
    let format_options = [
        "openai     — OpenAI-compatible chat API   (Ollama, LM Studio, vLLM, llama.cpp, …)",
        "anthropic  — Anthropic Messages API       (Ollama, llama.cpp, …)",
    ];
    let default_target = |format: usize| if format == 1 { ANTHROPIC_DEFAULT_TARGET } else { OPENAI_DEFAULT_TARGET };

    let mut slug = default_slug(&state.providers);
    let mut format = 0usize;
    let mut target = OPENAI_DEFAULT_TARGET.to_string();
    let mut key = String::new();
    // expose = a whitelist picked from the probed list; initial = ids typed by hand when
    // the target could not be reached. Both empty = all models, follow the local server.
    let mut expose: Vec<String> = Vec::new();
    let mut initial: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut notice: Option<String> = None;

    let answers = |slug: &str, format: usize, target: &str, key: &str, expose: &[String], initial: &[String]| -> AddAnswers {
        (
            slug.to_string(),
            if format == 1 { "anthropic" } else { "openai" }.to_string(),
            target.to_string(),
            (!key.is_empty()).then(|| key.to_string()),
            expose.to_vec(),
            initial.to_vec(),
        )
    };
    let models_text = |expose: &[String], initial: &[String]| {
        if !expose.is_empty() {
            format!("only {}", expose.join(", "))
        } else if !initial.is_empty() {
            format!("{} (typed by hand — the first connect reports the real list)", initial.join(", "))
        } else {
            "all — follows the local server".to_string()
        }
    };

    loop {
        let api_name = if format == 1 { "anthropic" } else { "openai" };
        let rows = vec![
            tui::RootRow { label: "Slug".into(), value: format!("{slug}   (models are called {slug}/<model>)"), changed: false },
            tui::RootRow { label: "API".into(), value: api_name.into(), changed: false },
            tui::RootRow { label: "Target".into(), value: target.clone(), changed: false },
            tui::RootRow {
                label: "Local API key".into(),
                value: if key.is_empty() { "none".into() } else { "set (kept on this device only)".into() },
                changed: false,
            },
            tui::RootRow { label: "Models".into(), value: models_text(&expose, &initial), changed: false },
        ];
        let status = notice.take().unwrap_or_else(|| format!("Not registered yet — Tab registers \"{slug}\" on {}", state.base_url));
        let (action, at) = tui::root(title, &[], &rows, cursor, &status)?;
        cursor = at;
        match action {
            tui::RootAction::Save => return Ok(answers(&slug, format, &target, &key, &expose, &initial)),
            tui::RootAction::Exit => {
                let changes = vec![
                    format!("Register \"{slug}\" ({api_name}) on {}", state.base_url),
                    format!("  Target  {target}"),
                    format!("  Models  {}", models_text(&expose, &initial)),
                ];
                if tui::exit_decision(title, &changes)? {
                    return Ok(answers(&slug, format, &target, &key, &expose, &initial));
                }
            }
            tui::RootAction::Edit(0) => {
                if let Nav::Next(v) = tui::input_with_note(title, &[], "Slug", &default_slug(&state.providers), &slug)? {
                    slug = v;
                }
            }
            tui::RootAction::Edit(1) => {
                if let Nav::Next(i) = tui::choose(title, &format_note, &format_options, format)? {
                    // A target still at the old API's default follows the new API's default.
                    if i != format && target == default_target(format) {
                        target = default_target(i).to_string();
                    }
                    format = i;
                }
            }
            tui::RootAction::Edit(2) => {
                let note: &[&str] = if format == 1 { &ANTHROPIC_TARGET_NOTE } else { &OPENAI_TARGET_NOTE };
                if let Nav::Next(v) = tui::input_with_note(title, note, "Target base URL", default_target(format), &target)? {
                    match normalize_target(&v) {
                        Ok(v) => target = v,
                        Err(e) => notice = Some(format!("Target not changed: {e}")),
                    }
                }
            }
            tui::RootAction::Edit(3) => {
                if let Nav::Next(v) = tui::input_secret(title, "Local API key (Enter for none)", &key)? {
                    key = v;
                }
            }
            tui::RootAction::Edit(_) => {
                let target_key = (!key.is_empty()).then(|| key.clone());
                match api::probe_local_models(api_name, &target, target_key.as_deref()).await {
                    Ok(models) if !models.is_empty() => {
                        match tui::pick_models(&format!("{title} — expose which models?"), &models, &expose)? {
                            // "All models (follow local server)": store no filter, so models
                            // added locally later appear automatically.
                            Nav::Next(None) => {
                                expose.clear();
                                initial.clear();
                            }
                            Nav::Next(Some(subset)) => {
                                expose = subset;
                                initial.clear();
                            }
                            _ => {}
                        }
                    }
                    _ => {
                        // A target that is not running yet must not block registration:
                        // ids by hand, which the first connect overwrites with the truth.
                        let typed = initial.join(",");
                        let note = [&*format!("Couldn't reach {target} — type model ids, or Enter on \"-\" for none."), ""];
                        if let Nav::Next(manual) = tui::input_with_note(title, &note, "Model ids, comma-separated", "-", &typed)? {
                            expose.clear();
                            initial = if manual == "-" {
                                Vec::new()
                            } else {
                                manual.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                            };
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// attach

/// Serve an already-registered provider from this device: the server row
/// exists (created by `add` on another machine), this just records where the
/// local server lives here. Any of the user's devices may hold the socket —
/// the last `start` to connect wins the provider (docs/cli.md).
pub async fn cmd_attach(
    file: &StateFile,
    slug: &str,
    target: Option<String>,
    target_key: Option<String>,
) -> Result<()> {
    let state = file.load()?;
    api::require_signed_in(&state)?;
    if state.providers.iter().any(|p| p.slug == slug) {
        bail!("\"{slug}\" is already set up on this device — edit {} or remove it first", file.path.display());
    }
    let target = normalize_target(&target.context("--target is required (where the local server lives here)")?)?;

    let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
    let access = tokens.get(false).await?;
    let remote = api::list_providers(&state.base_url, &access).await?;
    let item = remote
        .iter()
        .find(|r| r.slug == slug)
        .with_context(|| format!("no provider \"{slug}\" on the server — register a new one with `kano-proxy tunnel add`"))?;

    let provider = ProviderState {
        id: item.id.clone(),
        slug: item.slug.clone(),
        format: item.format.clone(),
        target,
        target_key,
        expose: Vec::new(),
    };
    mutate_state(file, |state| state.providers.push(provider))?;
    eprintln!("✓ attached \"{slug}\" — run `kano-proxy tunnel start` to serve it from this device");
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

    println!("{:<20} {:<10} {:<32} {:<11} {:>6}  LAST REPORT", "SLUG", "FORMAT", "TARGET", "STATE", "MODELS");
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
        println!("not signed in — run `kano-proxy login`");
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
    println!();
    agents::print_rows(&state);
    Ok(())
}

// ---------------------------------------------------------------------------
// logout

/// The reverse of `login` (docs/cli.md § Command surface): restore every agent
/// this device set up, revoke the device (its keys go with it), forget the
/// sign-in. A server that already forgot the device does not block the local
/// cleanup.
pub async fn cmd_logout(file: &StateFile, no_tui: bool, force: bool, remove_group: bool) -> Result<()> {
    let state = file.load()?;
    if !state.signed_in() && state.agents.is_empty() {
        bail!("this device is not signed in");
    }
    let managed: Vec<AgentKind> = AgentKind::ALL.iter().copied().filter(|k| state.agents.contains_key(k.id())).collect();
    let mut restored: Vec<&str> = Vec::new();
    for kind in managed {
        eprintln!("→ restoring {}", kind.label());
        let result = agents::cmd_restore(
            file,
            kind,
            RestoreArgs { no_tui, dry_run: false, full: false, force, backup: None, remove_group },
        )
        .await;
        match result {
            Ok(()) => restored.push(kind.label()),
            // Quitting part-way is not "nothing changed": say where it stopped.
            Err(e) if e.is::<tui::Cancelled>() && !restored.is_empty() => {
                return Err(tui::Cancelled(Some(format!(
                    "Cancelled — {} already restored; this device is still signed in. Run `kano-proxy logout` again to finish.",
                    restored.join(" and ")
                )))
                .into())
            }
            Err(e) => return Err(e),
        }
    }
    if state.signed_in() {
        let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
        match tokens.get(false).await {
            Ok(access) => match api::delete_device(&state.base_url, &access).await? {
                true => eprintln!("✓ device \"{}\" revoked", state.device_name.as_deref().unwrap_or("?")),
                false => eprintln!("· the server had already revoked this device"),
            },
            Err(_) => eprintln!("· the server no longer accepts this device — it was revoked already"),
        }
    }
    mutate_state(file, |s| {
        s.device_id = None;
        s.device_name = None;
        s.refresh_token = None;
        s.pending_login = None;
        s.providers.clear();
    })?;
    eprintln!("✓ signed out — {} keeps only the server address", file.path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// start

pub async fn cmd_start(file: StateFile, concurrency: usize) -> Result<()> {
    let state = file.load()?;
    api::require_signed_in(&state)?;
    if state.providers.is_empty() {
        bail!("no providers registered — run `kano-proxy tunnel add` first");
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
        // Note http:///v1 is NOT an error: the WHATWG parser skips extra
        // slashes before the authority, so it means host "v1" — same as a
        // browser. Only genuinely hostless shapes are refused.
        assert!(normalize_target("http://:11434/v1").is_err());
        assert!(normalize_target("http://user@/v1").is_err());
        assert!(normalize_target("http://?x").is_err());
    }

    #[test]
    fn add_screen_examples_are_valid_targets_of_the_right_shape() {
        for note in [&OPENAI_TARGET_NOTE[..], &ANTHROPIC_TARGET_NOTE[..]] {
            let openai = std::ptr::eq(note.as_ptr(), OPENAI_TARGET_NOTE.as_ptr());
            let examples: Vec<&str> = note.iter().filter_map(|l| l.split_whitespace().last()).filter(|w| w.starts_with("http")).collect();
            assert!(!examples.is_empty());
            for ex in examples {
                assert_eq!(normalize_target(ex).unwrap(), ex);
                assert_eq!(ex.ends_with("/v1"), openai, "{ex}");
            }
        }
        assert!(OPENAI_TARGET_NOTE.contains(&"  Ollama      http://localhost:11434/v1"));
        assert_eq!(normalize_target(OPENAI_DEFAULT_TARGET).unwrap(), OPENAI_DEFAULT_TARGET);
        assert_eq!(normalize_target(ANTHROPIC_DEFAULT_TARGET).unwrap(), ANTHROPIC_DEFAULT_TARGET);
        assert!(OPENAI_DEFAULT_TARGET.ends_with("/v1") && !ANTHROPIC_DEFAULT_TARGET.ends_with("/v1"));
    }

    #[test]
    fn server_prompt_prefers_flag_then_state_then_hosted_default() {
        assert_eq!(server_prompt_default(Some("https://a.example"), "https://b.example"), "https://a.example");
        assert_eq!(server_prompt_default(None, "https://b.example"), "https://b.example");
        assert_eq!(server_prompt_default(Some("  "), ""), DEFAULT_BASE_URL);
        assert_eq!(server_prompt_default(None, ""), DEFAULT_BASE_URL);
    }

    #[test]
    fn hosted_default_passes_the_cli_s_own_base_url_rules() {
        assert_eq!(api::normalize_base_url(DEFAULT_BASE_URL).unwrap(), DEFAULT_BASE_URL);
    }
}
