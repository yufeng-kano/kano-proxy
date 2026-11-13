//! `kano-proxy agent launch` (docs/cli.md § Command surface): start a coding
//! agent pointed at the proxy for one process. Nothing is written to the
//! agent's own config: Claude Code gets a settings file of the launch's own
//! through `--settings`, Codex gets `-c` overrides and its key in the child's
//! environment. The key is minted for the launch and revoked when the agent
//! exits; a launch that dies leaves a record the next launch sweeps.

use std::ffi::OsString;
use std::fmt::Display;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::doc::Doc;
use super::{claude_code, codex, group_models, now_stamp, provider_ids, AgentKind, EFFORTS, KEY_PLACEHOLDER, MAX_GROUP_MODELS};
use crate::api::{self, CatalogModel, Group, GroupModel, GroupTarget};
use crate::state::StateFile;
use crate::tunnel::TokenCache;

/// The environment variable Codex reads the launch's key from (`env_key`).
pub const CODEX_KEY_ENV: &str = "KANO_PROXY_API_KEY";
/// The provider id a launch defines. Not `agent setup`'s `kano`: `-c` overrides merge into
/// a table of the same id key by key, and a set-up machine's table carries a bearer token.
const CODEX_PROVIDER: &str = "kano-launch";
/// Set to the empty string — which Claude Code reads as unset — in the launch's settings
/// file. A `--settings` file only overrides the keys it names, so everything in the user's
/// own settings or shell that carries another credential, another endpoint, a provider
/// switch or a model pin has to be named to stay out of the session.
const CLAUDE_CLEARED: [&str; 12] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_EXTRA_BODY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
];
/// How long the agent gets to exit after the launch passed a stop signal on to it.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// A launch's own server calls are bounded: a dead network must not hold the terminal.
const MINT_TIMEOUT: Duration = Duration::from_secs(30);
const REVOKE_TIMEOUT: Duration = Duration::from_secs(15);
/// A record with no key id is a launch that has not minted yet; older than this it is one
/// that died there.
const UNMINTED_RECORD_AGE: Duration = Duration::from_secs(60);

pub struct LaunchArgs {
    pub dry_run: bool,
    /// Codex: the name it starts with (a model the group serves).
    pub model: Option<String>,
    /// Codex: low | medium | high | xhigh.
    pub effort: Option<String>,
    /// Everything after `--`, handed to the agent untouched.
    pub args: Vec<OsString>,
}

/// How the launch leaves: with the agent's exit code, or the way a signal ended it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
}

impl AgentKind {
    /// The executable the agent installs.
    pub fn program(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude",
            AgentKind::Codex => "codex",
        }
    }

    fn install_page(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "https://code.claude.com/docs/en/setup",
            AgentKind::Codex => "https://github.com/openai/codex",
        }
    }
}

/// `<state dir>/launch/` — the per-session files, beside the state file.
pub fn launch_dir(state_path: &Path) -> PathBuf {
    state_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".")).join("launch")
}

/// A line of the launch's own, on stderr. Never a panic: after a hangup the terminal is
/// gone, and the cleanup that follows matters more than the line.
fn say(line: impl Display) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

pub async fn cmd_launch(file: &StateFile, kind: AgentKind, args: LaunchArgs) -> Result<()> {
    match run(file, kind, args).await {
        Ok(Exit::Code(0)) => Ok(()),
        // The agent's own exit code, not one of ours (docs/cli.md § Runtime behavior).
        Ok(Exit::Code(code)) => std::process::exit(code),
        Ok(Exit::Signal(signal)) => signals::leave_by(signal),
        // Before the agent started, the usual codes: the device refused is 2.
        Err(e) if api::is_auth_rejection(&e) => {
            say(format_args!("error: {e:#}"));
            std::process::exit(crate::commands::EXIT_AUTH_REJECTED);
        }
        Err(e) => Err(e),
    }
}

/// The whole launch; returns how to leave.
pub async fn run(file: &StateFile, kind: AgentKind, args: LaunchArgs) -> Result<Exit> {
    check_args(kind, &args)?;
    let state = file.load()?;
    api::require_signed_in(&state)?;
    let base_url = state.base_url.clone();
    let device_id = state.device_id.clone().unwrap_or_default();
    let device_name = state.device_name.as_deref().unwrap_or("device");
    // At most 64 characters, the server's cap on a key name.
    let key_name: String = format!("{}-launch@{device_name}", kind.id()).chars().take(64).collect();
    let program = super::find_program(kind.program());
    if program.is_none() && !args.dry_run {
        bail!("no `{}` on PATH — install {} first ({})", kind.program(), kind.label(), kind.install_page());
    }
    // From here on a signal does not kill the launch outright: a key minted must be a key
    // revoked. Until there is a key, a signal simply cancels.
    let mut signals = signals::Signals::install()?;

    let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
    // Not cancellable: a rotation cut short can leave a refresh token the server has
    // already superseded. It is bounded on its own.
    let access = tokens.get(false).await?;
    // Absolute: Codex reads its catalog path against its own directory, not this one.
    let dir = launch_dir(&file.path);
    let dir = std::path::absolute(&dir).unwrap_or(dir);

    let prepare = async {
        if !args.dry_run {
            sweep(&dir, &base_url, &device_id, &access).await;
        }
        // The group: what it already holds stays exactly as it is. Models the account
        // gained since — which no name in the group serves yet — are added, each answering
        // as itself; a group the account does not have is created with setup's defaults.
        let catalog = api::list_models(&base_url, &access).await?;
        let groups = api::list_groups(&base_url, &access).await?;
        let (group, change) = match groups.into_iter().find(|g| g.slug == kind.group_slug()) {
            Some(g) => {
                let (new, left_out) = new_models(kind, &g, &catalog);
                let names: Vec<String> = new.iter().map(|m| m.name.clone()).collect();
                let mut models = g.models.clone();
                models.extend(new);
                if names.is_empty() {
                    (g, GroupChange::None { left_out })
                } else if args.dry_run {
                    (Group { models, ..g }, GroupChange::Added { names, left_out })
                } else {
                    match api::update_group_models(&base_url, &access, &g.id, &models).await {
                        Ok(updated) => (updated, GroupChange::Added { names, left_out }),
                        // Not worth failing a launch over: the group as it is still works.
                        Err(e) => {
                            say(format_args!("· could not add {} to the group \"{}\" ({e:#}) — launching with the group as it is", names.join(", "), g.slug));
                            (g, GroupChange::None { left_out: Vec::new() })
                        }
                    }
                }
            }
            None => {
                let models = default_group_models(kind, &base_url, &catalog)?;
                let group = if args.dry_run {
                    Group { id: String::new(), slug: kind.group_slug().to_string(), models }
                } else {
                    create_group(kind, &base_url, &access, &models).await?
                };
                (group, GroupChange::Created)
            }
        };
        if group.models.is_empty() {
            bail!(
                "the model group \"{}\" has no models — add some on the Groups page, or run `kano-proxy agent setup {}`",
                group.slug,
                kind.id()
            );
        }
        let plan = Plan::new(kind, &base_url, &group, &catalog, &args)?;
        Ok((group, change, plan))
    };
    let (group, change, plan) = tokio::select! {
        prepared = prepare => prepared?,
        // Nothing is minted yet: there is nothing to undo.
        got = signals.next() => return Ok(got.cancelled()),
    };

    if args.dry_run {
        println!("dry run — nothing is created, minted, revoked or started\n");
        println!(
            "Group \"{}\" — {}",
            group.slug,
            match &change {
                GroupChange::Created => "would be created with these models:".to_string(),
                GroupChange::Added { names, .. } => format!("what it holds is kept, and {} would be added (new on your account):", names.join(", ")),
                GroupChange::None { .. } => "used as it is:".to_string(),
            }
        );
        for model in &group.models {
            let targets: Vec<&str> = model.targets.iter().map(|t| t.model.as_str()).collect();
            println!("  {:<32} → {}", model.name, targets.join(", "));
        }
        if let Some(note) = change.left_out_note(&group) {
            println!("{note}");
        }
        for note in &plan.notes {
            println!("{note}");
        }
        println!("\nKey \"{key_name}\" — minted for this launch, revoked when {} exits\n", kind.label());
        let shown = program.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| format!("{} (not found on PATH)", kind.program()));
        let rest = shown_args(&args.args.iter().map(|a| a.to_string_lossy().to_string()).collect::<Vec<_>>());
        match &plan.agent {
            AgentPlan::ClaudeCode { .. } => {
                println!("{shown} --settings {} {rest}", shown_arg(&dir.join("<launch-id>.settings.json").display().to_string()));
                println!("\nThe settings file:\n{}", serde_json::to_string_pretty(&plan.claude_settings(&base_url, KEY_PLACEHOLDER))?);
            }
            AgentPlan::Codex { model, effort, .. } => {
                let overrides = codex_overrides(&base_url, model, effort.as_deref(), Some(&dir.join("<launch-id>.models.json")));
                println!("{CODEX_KEY_ENV}={KEY_PLACEHOLDER} {shown} {} {rest}", shown_args(&overrides));
            }
        }
        return Ok(Exit::Code(0));
    }
    let program = program.expect("checked above");
    say(format_args!(
        "✓ group \"{}\" — {} models{}",
        group.slug,
        group.models.len(),
        match &change {
            GroupChange::Created => " (created with the defaults; `kano-proxy agent setup` or the Groups page changes them)".to_string(),
            GroupChange::Added { names, .. } => format!(" (added {}, new on your account)", names.join(", ")),
            GroupChange::None { .. } => String::new(),
        }
    ));
    if let Some(note) = change.left_out_note(&group) {
        say(note);
    }
    for note in &plan.notes {
        say(note);
    }

    // The record first, locked, so a sweep running beside this launch leaves it alone.
    let mut record = Record::create(&dir, RecordBody { base_url: base_url.clone(), device_id, key_name: key_name.clone(), key_id: None })?;
    let minted = match tokio::time::timeout(MINT_TIMEOUT, api::create_key(&base_url, &access, &key_name)).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!("cannot reach {base_url} (no answer in {} s)", MINT_TIMEOUT.as_secs())),
    };
    let key = match minted {
        Ok(key) => key,
        Err(e) => {
            // Only an answer from the server proves there is no key.
            if !api::is_server_answer(&e) {
                say(format_args!("· the key may have been minted all the same — if \"{key_name}\" is on the Access page with no session running, revoke it there"));
            }
            record.finish(true);
            return Err(e);
        }
    };
    let outcome = match record.set_key(&key.id) {
        Err(e) => Err(e),
        Ok(()) => {
            say(format_args!("✓ key \"{}\" (revoked when {} exits)", key.name, kind.label()));
            start(kind, &program, &base_url, &plan, &args, &record, &key, &mut signals).await
        }
    };

    // Whatever happened above, the key goes and the files with it. No signal cuts this
    // short — the Ctrl-C that ended the agent is often still on its way here — so it is
    // bounded instead: when it cannot be done now the record stays, and the next launch
    // revokes.
    let revoked = revoke(file, &tokens, &record.body, &key.id).await;
    record.finish(revoked.is_ok());
    match &revoked {
        Ok(Gone::Revoked) => say(format_args!("✓ revoked key \"{}\"", key.name)),
        Ok(Gone::WithDevice) => say(format_args!("· key \"{}\" went with this device, which was signed out or revoked while the agent ran", key.name)),
        Err(why) => say(format_args!(
            "· could not revoke key \"{}\" ({why}) — the next `kano-proxy agent launch` retries, or revoke it on the Access page",
            key.name
        )),
    }
    outcome
}

/// How a launch's key came to be gone.
enum Gone {
    Revoked,
    /// The sign-in it belonged to is gone, and with a device go the keys it minted.
    WithDevice,
}

/// Revoke the launch's key when the agent has exited. The token is refreshed now, never
/// reused: the session may have outlived the one it started with (a laptop that slept
/// does not notice), and a `401` on the delete itself must not be read as "already gone".
async fn revoke(file: &StateFile, tokens: &TokenCache, body: &RecordBody, key_id: &str) -> std::result::Result<Gone, String> {
    let state = file.load().map_err(|e| format!("{e:#}"))?;
    if state.base_url != body.base_url || state.device_id.as_deref() != Some(body.device_id.as_str()) {
        // Signed out (or in again as another device) during the session: nothing here
        // can revoke the key any more, and nothing needs to if the device is gone.
        return Ok(Gone::WithDevice);
    }
    let access = match tokens.get(true).await {
        Ok(access) => access,
        Err(e) if api::is_auth_rejection(&e) => return Ok(Gone::WithDevice),
        Err(e) => return Err(format!("{e:#}")),
    };
    match tokio::time::timeout(REVOKE_TIMEOUT, api::delete_key(&body.base_url, &access, key_id)).await {
        Ok(Ok(_)) => Ok(Gone::Revoked),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(_) => Err(format!("no answer in {} s", REVOKE_TIMEOUT.as_secs())),
    }
}

/// What can be refused before anything is read or minted.
fn check_args(kind: AgentKind, args: &LaunchArgs) -> Result<()> {
    let has = |flag: &str| args.args.iter().filter_map(|a| a.to_str()).any(|a| a == flag || a.strip_prefix(flag).is_some_and(|rest| rest.starts_with('=')));
    match kind {
        AgentKind::ClaudeCode => {
            if args.model.is_some() || args.effort.is_some() {
                bail!("--model and --effort are for codex — hand Claude Code its own flag instead: `kano-proxy agent launch claude-code -- --model <name>`");
            }
            if has("--settings") {
                bail!("--settings cannot be passed to a launched Claude Code: the launch passes its own, and two would fight");
            }
            if has("--bg") || has("--background") {
                bail!("a launched Claude Code cannot run in the background: its key is revoked when this command returns");
            }
        }
        AgentKind::Codex => {
            if let Some(effort) = args.effort.as_deref() {
                if !EFFORTS.contains(&effort) {
                    bail!("--effort {effort}: use one of {}", EFFORTS.join(", "));
                }
            }
            for flag in ["--oss", "--local-provider", "--remote"] {
                if has(flag) {
                    bail!("{flag} takes Codex off the proxy — start `codex` itself for that");
                }
            }
        }
    }
    Ok(())
}

/// An argument as a dry run prints it: quoted when a shell would split or expand it.
fn shown_arg(arg: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "-_=+.,:/@%".contains(c);
    if !arg.is_empty() && arg.chars().all(plain) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

fn shown_args(args: &[String]) -> String {
    args.iter().map(|a| shown_arg(a)).collect::<Vec<_>>().join(" ")
}

/// What `agent setup` starts from, for an account that has no group yet.
fn default_group_models(kind: AgentKind, base_url: &str, catalog: &[CatalogModel]) -> Result<Vec<GroupModel>> {
    let models = match kind {
        AgentKind::ClaudeCode => {
            let slots = claude_code::SLOTS.iter().map(|(_, family, _)| claude_code::default_targets(catalog, family)).collect();
            claude_code::plan_from(base_url, catalog, &claude_code::Picks { slots }).group_models
        }
        AgentKind::Codex => {
            let ids = provider_ids(catalog, kind.provider());
            match ids.first() {
                Some(first) => group_models(kind.provider(), &ids, &[(first.clone(), vec![format!("codex/{first}")])]),
                None => Vec::new(),
            }
        }
    };
    if models.is_empty() {
        bail!(match kind {
            AgentKind::ClaudeCode =>
                "your account has no Claude model and no `claude-code` group — connect a provider on the Providers page, or pick other targets with `kano-proxy agent setup claude-code`",
            AgentKind::Codex => "your account has no codex models — connect a ChatGPT subscription on the Providers page first",
        });
    }
    Ok(models)
}

/// What a launch did to the agent's group.
enum GroupChange {
    Created,
    /// Models new on the account were added; nothing the group held was touched.
    Added { names: Vec<String>, left_out: Vec<String> },
    None { left_out: Vec<String> },
}

impl GroupChange {
    /// Models the account has that the group has no room for.
    fn left_out_note(&self, group: &Group) -> Option<String> {
        let left_out = match self {
            GroupChange::Added { left_out, .. } | GroupChange::None { left_out } => left_out,
            GroupChange::Created => return None,
        };
        (!left_out.is_empty()).then(|| {
            format!(
                "· the group \"{}\" is full ({MAX_GROUP_MODELS} models): {} on your account {} not in it — make room on the Groups page",
                group.slug,
                left_out.join(", "),
                if left_out.len() == 1 { "is" } else { "are" }
            )
        })
    }
}

/// The agent's own provider's models that no name in the group serves — in practice what
/// the account gained since the group was written, and also a mirror the user deleted —
/// each answering as itself, in catalog order, as far as the group's cap allows; and the
/// ids there was no room for.
fn new_models(kind: AgentKind, group: &Group, catalog: &[CatalogModel]) -> (Vec<GroupModel>, Vec<String>) {
    let room = MAX_GROUP_MODELS.saturating_sub(group.models.len());
    let mut missing: Vec<String> = Vec::new();
    for id in provider_ids(catalog, kind.provider()) {
        if !group.models.iter().any(|m| m.name == id) && !missing.contains(&id) {
            missing.push(id);
        }
    }
    let left_out = missing.split_off(room.min(missing.len()));
    let new = missing
        .into_iter()
        .map(|id| GroupModel { targets: vec![GroupTarget { model: format!("{}/{id}", kind.provider()), account_id: None }], name: id })
        .collect();
    (new, left_out)
}

/// Create the agent's group. Another launch may have created it a moment ago: then that
/// one is used.
async fn create_group(kind: AgentKind, base_url: &str, access: &str, models: &[GroupModel]) -> Result<Group> {
    match api::create_group(base_url, access, kind.group_name(), kind.group_slug(), models).await {
        Ok(group) => Ok(group),
        Err(e) => match api::list_groups(base_url, access).await.ok().and_then(|all| all.into_iter().find(|g| g.slug == kind.group_slug())) {
            Some(group) => Ok(group),
            None => Err(e.context(format!(
                "cannot create the model group \"{}\" — if a group named \"{}\" exists under another slug, run `kano-proxy agent setup {}` or rename it on the Groups page",
                kind.group_slug(),
                kind.group_name(),
                kind.id()
            ))),
        },
    }
}

// ---------------------------------------------------------------------------
// plans

/// What this launch hands the agent, decided before anything is minted.
struct Plan {
    agent: AgentPlan,
    /// Lines the launch prints about choices it made for the user.
    notes: Vec<String>,
}

enum AgentPlan {
    ClaudeCode {
        /// The name each slot is served under, in `SLOTS` order.
        slots: Vec<String>,
        /// The model to start on, when the user's saved one is a name the group cannot serve.
        model: Option<String>,
    },
    Codex {
        model: String,
        effort: Option<String>,
        /// The group's names — what the model catalog is kept to.
        served: Vec<String>,
    },
}

impl Plan {
    fn new(kind: AgentKind, base_url: &str, group: &Group, catalog: &[CatalogModel], args: &LaunchArgs) -> Result<Self> {
        // The user's own config is read, never written. Unreadable is the same as absent.
        let own = Doc::load(kind.doc_kind(), &own_config_path(kind)).ok();
        let field = |name: &str| own.as_ref().and_then(|d| d.get(name)).and_then(|v| v.as_str().map(str::to_string));
        let mut notes = Vec::new();
        let agent = match kind {
            AgentKind::ClaudeCode => {
                let set_up_here = field("env.ANTHROPIC_BASE_URL").as_deref() == Some(kind.group_base(base_url).as_str());
                let written: Vec<Option<String>> =
                    claude_code::SLOTS.iter().map(|(_, _, f)| if set_up_here { field(f) } else { None }).collect();
                let (slots, borrowed) = slot_names(group, catalog, &written);
                for (index, from) in borrowed {
                    notes.push(format!(
                        "· {} slot: the group has no {} model — served by {from}",
                        claude_code::SLOTS[index].0,
                        claude_code::SLOTS[index].1
                    ));
                }
                let saved = field("model");
                let model = start_model(saved.as_deref(), group, &slots);
                if let Some(model) = &model {
                    notes.push(format!(
                        "· starting on {model}: the model saved in your settings ({}) is not one this group serves",
                        saved.unwrap_or_default()
                    ));
                }
                AgentPlan::ClaudeCode { slots, model }
            }
            AgentKind::Codex => {
                let set_up_here = field("model_providers.kano.base_url").as_deref() == Some(kind.group_base(base_url).as_str());
                let written = if set_up_here { field("model") } else { None };
                let model = codex_model(group, catalog, args.model.as_deref(), written.as_deref())?;
                let effort = match (args.effort.clone(), field("model_reasoning_effort")) {
                    (Some(effort), _) => Some(effort),
                    (None, Some(own)) if !EFFORTS.contains(&own.as_str()) => {
                        notes.push(format!("· reasoning effort high: your config's \"{own}\" is not one the proxy accepts ({})", EFFORTS.join(", ")));
                        Some("high".to_string())
                    }
                    (None, _) => None,
                };
                AgentPlan::Codex { model, effort, served: group.models.iter().map(|m| m.name.clone()).collect() }
            }
        };
        Ok(Self { agent, notes })
    }

    fn claude_settings(&self, base_url: &str, key: &str) -> Value {
        match &self.agent {
            AgentPlan::ClaudeCode { slots, model } => claude_settings(base_url, slots, model.as_deref(), key),
            AgentPlan::Codex { .. } => Value::Null,
        }
    }
}

/// The config file the agent about to start will read: its own directory override
/// (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) when the environment sets one, else the usual place.
fn own_config_path(kind: AgentKind) -> PathBuf {
    let usual = kind.config_path();
    let variable = match kind {
        AgentKind::ClaudeCode => "CLAUDE_CONFIG_DIR",
        AgentKind::Codex => "CODEX_HOME",
    };
    match (std::env::var_os(variable).filter(|v| !v.is_empty()), usual.file_name()) {
        (Some(dir), Some(name)) => PathBuf::from(dir).join(name),
        _ => usual,
    }
}

/// The name each of Claude Code's slots is served under, in `SLOTS` order, and which slots
/// had to borrow another's name (`(slot index, the name borrowed)`). Order-free, the way
/// `agent setup` derives a name: what setup wrote on this machine (`written`), when the
/// group still serves it; else the account's own `claude-code` model of the family; else
/// the first group name carrying the family word. A slot none of that names is served by
/// the nearest slot that has one — a cheaper one first — so every alias resolves.
pub fn slot_names(group: &Group, catalog: &[CatalogModel], written: &[Option<String>]) -> (Vec<String>, Vec<(usize, String)>) {
    let serves = |name: &str| group.models.iter().any(|m| m.name == name);
    let own = provider_ids(catalog, AgentKind::ClaudeCode.provider());
    let named: Vec<Option<String>> = claude_code::SLOTS
        .iter()
        .enumerate()
        .map(|(i, (_, family, _))| {
            written
                .get(i)
                .cloned()
                .flatten()
                .filter(|name| serves(name))
                .or_else(|| own.iter().find(|id| id.to_lowercase().contains(family)).filter(|id| serves(id)).cloned())
                .or_else(|| group.models.iter().map(|m| &m.name).find(|n| n.to_lowercase().contains(family)).cloned())
        })
        .collect();
    let first = group.models.first().map(|m| m.name.clone()).unwrap_or_default();
    let mut borrowed = Vec::new();
    let slots = (0..named.len())
        .map(|i| match &named[i] {
            Some(name) => name.clone(),
            None => {
                let from = (i + 1..named.len()).chain((0..i).rev()).find_map(|j| named[j].clone()).unwrap_or_else(|| first.clone());
                borrowed.push((i, from.clone()));
                from
            }
        })
        .collect();
    (slots, borrowed)
}

/// The model a launched Claude Code starts on, when the one the user saved cannot work
/// here: an alias (`opus`, `sonnet[1m]`, `default`…) resolves through the slots and a name
/// the group serves is fine as it is — anything else is another gateway's id, and the
/// session starts on the Opus slot instead.
fn start_model(saved: Option<&str>, group: &Group, slots: &[String]) -> Option<String> {
    let saved = saved?.trim();
    let bare = saved.split('[').next().unwrap_or(saved).trim();
    let alias = !bare.is_empty() && bare.chars().all(|c| c.is_ascii_alphabetic());
    if alias || group.models.iter().any(|m| m.name == bare) {
        return None;
    }
    slots.get(1).or(slots.first()).cloned()
}

/// The settings file a launched Claude Code reads through `--settings`: the keys `agent
/// setup` writes, a name for every slot, no way into the background (the key does not
/// outlive this command), and everything that must not leak in set to "".
pub fn claude_settings(base_url: &str, slots: &[String], model: Option<&str>, key: &str) -> Value {
    let name = |field: &str| field.strip_prefix("env.").unwrap_or(field).to_string();
    let mut env = Map::new();
    env.insert("ANTHROPIC_BASE_URL".into(), json!(AgentKind::ClaudeCode.group_base(base_url)));
    env.insert(name(claude_code::KEY_FIELD), json!(key));
    env.insert(name(claude_code::DISCOVERY_FIELD), json!("1"));
    for ((_, _, field), slot) in claude_code::SLOTS.iter().zip(slots) {
        env.insert(name(field), json!(slot));
    }
    for cleared in CLAUDE_CLEARED {
        env.insert(cleared.into(), json!(""));
    }
    let mut settings = Map::new();
    settings.insert(claude_code::CONNECTORS_FIELD.into(), json!(true));
    settings.insert("disableAgentView".into(), json!(true));
    if let Some(model) = model {
        settings.insert("model".into(), json!(model));
    }
    settings.insert("env".into(), Value::Object(env));
    Value::Object(settings)
}

/// The model a launched Codex starts with: `--model`; else the one setup wrote on this
/// machine (`written`); else the account's first `codex` model, which is what setup starts
/// from; else the group's first — each only when the group serves it.
fn codex_model(group: &Group, catalog: &[CatalogModel], flag: Option<&str>, written: Option<&str>) -> Result<String> {
    let names: Vec<&str> = group.models.iter().map(|m| m.name.as_str()).collect();
    if let Some(model) = flag.map(str::trim).filter(|s| !s.is_empty()) {
        if !names.contains(&model) {
            bail!("--model {model}: not a model the group \"{}\" serves (one of: {})", group.slug, names.join(", "));
        }
        return Ok(model.to_string());
    }
    let own = provider_ids(catalog, AgentKind::Codex.provider());
    Ok(written
        .filter(|w| names.contains(w))
        .or_else(|| own.iter().map(String::as_str).find(|id| names.contains(id)))
        .or_else(|| names.first().copied())
        .unwrap_or_default()
        .to_string())
}

/// A `-c` value as Codex will read it: Codex parses the value as TOML and takes anything
/// that does not parse as the literal string. Bare where that already yields the string,
/// a quoted TOML string otherwise — so no quote has to survive a Windows `.cmd` shim.
pub fn toml_arg(value: &str) -> String {
    match format!("_x_ = {value}").parse::<toml_edit::DocumentMut>() {
        Ok(_) => toml_edit::Value::from(value).to_string(),
        Err(_) => value.to_string(),
    }
}

/// Codex's configuration for this process: `-c` overrides, which rank above the user's and
/// the project's config files. The key is not among them — it travels in `CODEX_KEY_ENV`.
pub fn codex_overrides(base_url: &str, model: &str, effort: Option<&str>, catalog: Option<&Path>) -> Vec<String> {
    let table = format!("model_providers.{CODEX_PROVIDER}");
    let mut pairs: Vec<(String, String)> = vec![
        ("model_provider".into(), CODEX_PROVIDER.into()),
        (format!("{table}.name"), "Kano Proxy".into()),
        (format!("{table}.base_url"), AgentKind::Codex.group_base(base_url)),
        (format!("{table}.wire_api"), "responses".into()),
        (format!("{table}.env_key"), CODEX_KEY_ENV.into()),
        ("model".into(), model.into()),
    ];
    if let Some(effort) = effort {
        pairs.push(("model_reasoning_effort".into(), effort.into()));
    }
    if let Some(path) = catalog {
        pairs.push(("model_catalog_json".into(), path.display().to_string()));
    }
    pairs.into_iter().flat_map(|(k, v)| ["-c".to_string(), format!("{k}={}", toml_arg(&v))]).collect()
}

/// Write the launch's files, start the agent and wait for it.
#[allow(clippy::too_many_arguments)]
async fn start(
    kind: AgentKind,
    program: &Path,
    base_url: &str,
    plan: &Plan,
    args: &LaunchArgs,
    record: &Record,
    key: &api::CreatedKey,
    signals: &mut signals::Signals,
) -> Result<Exit> {
    let mut command = tokio::process::Command::new(program);
    match &plan.agent {
        AgentPlan::ClaudeCode { .. } => {
            let path = record.sibling("settings.json");
            let text = format!("{}\n", serde_json::to_string_pretty(&plan.claude_settings(base_url, &key.key))?);
            write_new_private(&path, text.as_bytes())?;
            command.arg("--settings").arg(&path);
        }
        AgentPlan::Codex { model, effort, served } => {
            let catalog = match codex::bundled_catalog() {
                Ok(bundled) => {
                    let path = record.sibling("models.json");
                    let text = format!("{}\n", serde_json::to_string_pretty(&json!({ "models": codex::transform_catalog(&bundled, served) }))?);
                    write_new_private(&path, text.as_bytes())?;
                    Some(path)
                }
                Err(e) => {
                    say(format_args!("· no model catalog: {e:#} — Codex runs on its fallback metadata, which is harmless"));
                    None
                }
            };
            command.args(codex_overrides(base_url, model, effort.as_deref(), catalog.as_deref()));
            command.env(CODEX_KEY_ENV, &key.key);
        }
    }
    command.args(&args.args);
    // Stopped before the agent started — a Ctrl-C it never saw: nothing runs, the key goes.
    if let Some(got) = signals.arrived() {
        return Ok(got.cancelled());
    }
    let mut child = command.spawn().with_context(|| format!("cannot start {}", kind.label()))?;
    loop {
        tokio::select! {
            status = child.wait() => return Ok(exit_of(status?)),
            got = signals.next() => match got {
                // Ctrl-C and Ctrl-\ are the agent's: the terminal sent them to it too.
                signals::Got::Agents(_) => {}
                // Told to stop: pass it on, give the agent a moment, then end it.
                signals::Got::Stop(signal) => {
                    signals::forward(&child, signal);
                    if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_err() {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                    }
                    return Ok(Exit::Signal(signal));
                }
            },
        }
    }
}

/// The agent's exit code as it gave it, or the signal that ended it.
fn exit_of(status: ExitStatus) -> Exit {
    if let Some(code) = status.code() {
        return Exit::Code(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Exit::Signal(signal);
        }
    }
    Exit::Code(1)
}

// ---------------------------------------------------------------------------
// signals

#[cfg(unix)]
mod signals {
    use std::future::pending;

    use anyhow::Result;
    use futures_util::FutureExt;
    use tokio::process::Child;
    use tokio::signal::unix::{signal, Signal, SignalKind};

    use super::Exit;

    pub enum Got {
        /// `SIGINT` / `SIGQUIT`: the terminal delivered it to the agent as well.
        Agents(i32),
        /// `SIGTERM` / `SIGHUP`: the launch is being told to stop.
        Stop(i32),
    }

    impl Got {
        /// The same signal, arriving before the agent started.
        pub fn cancelled(self) -> Exit {
            match self {
                Got::Agents(n) | Got::Stop(n) => Exit::Signal(n),
            }
        }
    }

    /// One stream per signal the launch must outlive. A signal that was already ignored
    /// when the launch started (`nohup`, a background job of a script) stays ignored: no
    /// handler is installed, and the agent inherits the same.
    pub struct Signals {
        interrupt: Option<Signal>,
        quit: Option<Signal>,
        terminate: Option<Signal>,
        hangup: Option<Signal>,
    }

    fn ignored(signal: libc::c_int) -> bool {
        // SAFETY: a null new action only reads the current disposition into `old`.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut old) == 0 && old.sa_sigaction == libc::SIG_IGN
        }
    }

    fn stream(number: libc::c_int, kind: SignalKind) -> Result<Option<Signal>> {
        Ok(if ignored(number) { None } else { Some(signal(kind)?) })
    }

    async fn recv(stream: &mut Option<Signal>) {
        match stream {
            Some(stream) => {
                stream.recv().await;
            }
            None => pending().await,
        }
    }

    impl Signals {
        /// Handlers, not "ignore": a handler is reset in the child, so the agent keeps its
        /// own Ctrl-C.
        pub fn install() -> Result<Self> {
            Ok(Self {
                interrupt: stream(libc::SIGINT, SignalKind::interrupt())?,
                quit: stream(libc::SIGQUIT, SignalKind::quit())?,
                terminate: stream(libc::SIGTERM, SignalKind::terminate())?,
                hangup: stream(libc::SIGHUP, SignalKind::hangup())?,
            })
        }

        pub async fn next(&mut self) -> Got {
            tokio::select! {
                _ = recv(&mut self.interrupt) => Got::Agents(libc::SIGINT),
                _ = recv(&mut self.quit) => Got::Agents(libc::SIGQUIT),
                _ = recv(&mut self.terminate) => Got::Stop(libc::SIGTERM),
                _ = recv(&mut self.hangup) => Got::Stop(libc::SIGHUP),
            }
        }

        /// A signal that already arrived, without waiting for one.
        pub fn arrived(&mut self) -> Option<Got> {
            self.next().now_or_never()
        }
    }

    /// Pass a stop signal on to the agent, which a signal sent to the launch alone
    /// (`kill`, a service manager) never reached.
    pub fn forward(child: &Child, signal: i32) {
        if let Some(pid) = child.id() {
            // SAFETY: the pid is this launch's own child, still unreaped while `child` holds it.
            unsafe {
                libc::kill(pid as libc::pid_t, signal);
            }
        }
    }

    /// Leave the way the signal would have ended the launch, so a shell loop around it
    /// stops on Ctrl-C; `128 + n` for a signal that is not safe to raise again.
    pub fn leave_by(signal: i32) -> ! {
        if [libc::SIGINT, libc::SIGTERM, libc::SIGHUP].contains(&signal) {
            // SAFETY: restoring the default action and raising a signal on this process.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
        }
        std::process::exit(128 + signal)
    }
}

#[cfg(windows)]
mod signals {
    use anyhow::Result;
    use futures_util::FutureExt;
    use tokio::process::Child;
    use tokio::signal::windows::{ctrl_break, ctrl_c, CtrlBreak, CtrlC};

    use super::Exit;

    #[allow(dead_code)]
    pub enum Got {
        /// Ctrl-C / Ctrl-Break: the console delivered it to the agent as well.
        Agents(i32),
        /// Never produced on Windows: there is no signal that asks the launch alone to stop.
        Stop(i32),
    }

    impl Got {
        pub fn cancelled(self) -> Exit {
            Exit::Code(130)
        }
    }

    pub struct Signals {
        ctrl_c: CtrlC,
        ctrl_break: CtrlBreak,
    }

    impl Signals {
        pub fn install() -> Result<Self> {
            Ok(Self { ctrl_c: ctrl_c()?, ctrl_break: ctrl_break()? })
        }

        pub async fn next(&mut self) -> Got {
            tokio::select! {
                _ = self.ctrl_c.recv() => Got::Agents(2),
                _ = self.ctrl_break.recv() => Got::Agents(2),
            }
        }

        pub fn arrived(&mut self) -> Option<Got> {
            self.next().now_or_never()
        }
    }

    pub fn forward(_child: &Child, _signal: i32) {}

    pub fn leave_by(signal: i32) -> ! {
        std::process::exit(128 + signal)
    }
}

// ---------------------------------------------------------------------------
// records

/// What a record says about its launch. Never the plaintext key.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecordBody {
    /// The sign-in the key belongs to: only a launch signed in as the same device against
    /// the same server can revoke it, so only that one touches the record.
    base_url: String,
    device_id: String,
    key_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key_id: Option<String>,
}

/// One launch's record: `<launch-id>.lock`, held under an exclusive OS lock for as long as
/// the launch lives. The OS drops the lock when the process dies, however it dies — which
/// is how the next launch tells a dead launch from a running one.
struct Record {
    path: PathBuf,
    /// The handle the lock is on. Everything is written through it, in place: replacing
    /// the file would leave the lock on a file nobody can see.
    lock: fs::File,
    body: RecordBody,
}

impl Record {
    fn create(dir: &Path, body: RecordBody) -> Result<Self> {
        create_private_dir(dir)?;
        let base = format!("{}-{}", now_stamp(), std::process::id());
        for n in 0..100 {
            let id = if n == 0 { base.clone() } else { format!("{base}-{n}") };
            let path = dir.join(format!("{id}.lock"));
            match private_options().create_new(true).open(&path) {
                Ok(lock) => {
                    // Blocking: a sweep may hold a record it is looking at for an instant.
                    if let Err(e) = lock.lock() {
                        say(format_args!("· {} cannot be locked ({e}) — if this launch dies, its key is not revoked automatically", dir.display()));
                    }
                    let mut record = Self { path, lock, body };
                    record.write()?;
                    return Ok(record);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("cannot create {}", path.display())),
            }
        }
        bail!("cannot create a launch record in {}", dir.display())
    }

    /// In place, and never shorter first: the record only ever grows (it gains the key
    /// id), so a write cut short leaves the old text's head, not an empty file.
    fn write(&mut self) -> Result<()> {
        let text = serde_json::to_vec(&self.body)?;
        self.lock.seek(SeekFrom::Start(0))?;
        self.lock.write_all(&text)?;
        self.lock.set_len(text.len() as u64)?;
        self.lock.sync_all()?;
        Ok(())
    }

    fn set_key(&mut self, key_id: &str) -> Result<()> {
        self.body.key_id = Some(key_id.to_string());
        self.write().with_context(|| format!("cannot write {}", self.path.display()))
    }

    /// `<launch-id>.<suffix>` beside the record.
    fn sibling(&self, suffix: &str) -> PathBuf {
        self.path.with_extension(suffix)
    }

    /// The session is over: its files go. The record itself stays when the key could not
    /// be revoked — unlocked once this returns, so the next launch's sweep picks it up.
    fn finish(self, revoked: bool) {
        remove_files(&self.path, revoked);
    }
}

/// Remove a record's sibling files, and the record itself when `with_record`.
fn remove_files(record: &Path, with_record: bool) {
    for suffix in ["settings.json", "models.json"] {
        let _ = fs::remove_file(record.with_extension(suffix));
    }
    if with_record {
        let _ = fs::remove_file(record);
    }
}

/// A record no living launch holds, locked by whoever found it.
struct Dead {
    path: PathBuf,
    /// `None`: not a record this version wrote.
    body: Option<RecordBody>,
    _lock: fs::File,
}

/// The records in `dir` no living launch holds, and how many a running launch holds. Only
/// "would block" means alive; a record that cannot be locked for any other reason, or
/// that is still being written, is left out of both.
fn records(dir: &Path) -> (Vec<Dead>, usize) {
    let mut dead = Vec::new();
    let mut live = 0;
    let Ok(entries) = fs::read_dir(dir) else { return (dead, live) };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        let Ok(mut file) = fs::OpenOptions::new().read(true).write(true).open(&path) else { continue };
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                live += 1;
                continue;
            }
            Err(fs::TryLockError::Error(_)) => continue,
        }
        let mut text = String::new();
        if file.seek(SeekFrom::Start(0)).is_err() || file.read_to_string(&mut text).is_err() {
            continue;
        }
        // A key id is `key_…` and nothing else: it goes into a request path, and a record
        // is only a file — one that says otherwise is not a record.
        let body: Option<RecordBody> = serde_json::from_str::<RecordBody>(&text).ok().filter(|b| b.key_id.as_deref().is_none_or(is_key_id));
        if body.as_ref().is_none_or(|b| b.key_id.is_none()) {
            // A launch that has just created its record — unless it has been so a while.
            let age = entry.metadata().and_then(|m| m.modified()).ok().and_then(|at| SystemTime::now().duration_since(at).ok());
            if age.is_none_or(|age| age < UNMINTED_RECORD_AGE) {
                continue;
            }
        }
        dead.push(Dead { path, body, _lock: file });
    }
    (dead, live)
}

fn is_key_id(id: &str) -> bool {
    id.starts_with("key_") && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Revoke the keys of launches that died and delete their files (docs/cli.md § `agent
/// launch`). Never fatal: what cannot be revoked now stays for the next launch. A record
/// of an earlier sign-in of this machine cannot be revoked from here at all — its
/// credentials are gone — so it is deleted with a line saying where the key can be.
async fn sweep(dir: &Path, base_url: &str, device_id: &str, access: &str) {
    for dead in records(dir).0 {
        let Some(body) = &dead.body else {
            remove_files(&dead.path, true);
            say("· a launch record could not be read and was deleted — a key named \"…-launch@…\" on the Access page with no session running can be revoked there");
            continue;
        };
        if body.base_url != base_url || body.device_id != device_id {
            remove_files(&dead.path, true);
            say(format_args!(
                "· key \"{}\" was left by a launch under an earlier sign-in ({}) and cannot be revoked from this one — if that device is still on the Access page, revoke it there",
                body.key_name, body.base_url
            ));
            continue;
        }
        let Some(key_id) = &body.key_id else {
            remove_files(&dead.path, true);
            say(format_args!(
                "· a launch died while minting its key — if \"{}\" is on the Access page with no session running, revoke it there",
                body.key_name
            ));
            continue;
        };
        match tokio::time::timeout(REVOKE_TIMEOUT, api::delete_key(base_url, access, key_id)).await {
            Ok(Ok(_)) => {
                remove_files(&dead.path, true);
                say(format_args!("✓ revoked key \"{}\", left by a launch that did not exit cleanly", body.key_name));
            }
            _ => {
                // The plaintext goes either way; the record waits for the next launch.
                remove_files(&dead.path, false);
                say(format_args!("· key \"{}\", left by a launch that did not exit cleanly, could not be revoked now", body.key_name));
            }
        }
    }
}

/// `kano-proxy logout`, once the server confirmed the device gone — its keys went with it,
/// so this sign-in's dead records only need deleting.
pub fn forget_dead(state_path: &Path, base_url: &str, device_id: &str) {
    for dead in records(&launch_dir(state_path)).0 {
        if dead.body.as_ref().is_none_or(|b| b.base_url == base_url && b.device_id == device_id) {
            remove_files(&dead.path, true);
        }
    }
}

/// `(running, left by a launch that died)` — for `kano-proxy status` and `logout`.
pub fn counts(state_path: &Path) -> (usize, usize) {
    let (dead, live) = records(&launch_dir(state_path));
    (live, dead.len())
}

fn private_options() -> fs::OpenOptions {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// A new file, `0600` from the moment it exists — it may hold the key.
fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = private_options().create_new(true).open(path).with_context(|| format!("cannot write {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// `0700`, also when it already existed looser: the names in it are the launches running.
fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).with_context(|| format!("cannot restrict {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::GroupTarget;

    fn group(names: &[&str]) -> Group {
        Group {
            id: "mgrp_1".into(),
            slug: "claude-code".into(),
            models: names
                .iter()
                .map(|n| GroupModel { name: n.to_string(), targets: vec![GroupTarget { model: format!("claude-code/{n}"), account_id: None }] })
                .collect(),
        }
    }

    fn catalog(ids: &[&str]) -> Vec<CatalogModel> {
        ids.iter()
            .map(|id| {
                let (p, u) = id.split_once('/').unwrap();
                CatalogModel { id: id.to_string(), provider: p.into(), upstream: u.into() }
            })
            .collect()
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    const CLAUDE: [&str; 5] = [
        "claude-code/claude-fable-5-1",
        "claude-code/claude-opus-5",
        "claude-code/claude-opus-4-8",
        "claude-code/claude-sonnet-5",
        "claude-code/claude-haiku-4-5",
    ];

    #[test]
    fn a_slot_is_named_the_way_setup_names_it_whatever_order_the_group_comes_in() {
        // The server does not promise the group's order: the account's own model of the
        // family names the slot, wherever it sits.
        let g = group(&["my-alias", "claude-opus-4-8", "claude-haiku-4-5", "claude-sonnet-5", "claude-opus-5", "claude-fable-5-1"]);
        let (slots, borrowed) = slot_names(&g, &catalog(&CLAUDE), &[None, None, None, None]);
        assert_eq!(slots, ["claude-fable-5-1", "claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"], "Fable, Opus, Sonnet, Haiku");
        assert!(borrowed.is_empty());

        // On a machine setup wrote, its own names win — a Fable slot kept on Opus stays there.
        let written = [Some("claude-opus-5".to_string()), Some("claude-opus-4-8".to_string()), None, Some("gone-from-the-group".to_string())];
        let (slots, _) = slot_names(&g, &catalog(&CLAUDE), &written);
        assert_eq!(slots, ["claude-opus-5", "claude-opus-4-8", "claude-sonnet-5", "claude-haiku-4-5"], "a name the group dropped falls back");

        // No claude-code account: the group's own name carrying the family word.
        let g = group(&["claude-opus-5-high", "claude-sonnet-5-thinking"]);
        let (slots, borrowed) = slot_names(&g, &catalog(&["antigravity/claude-opus-5-high"]), &[None, None, None, None]);
        assert_eq!(slots, ["claude-opus-5-high", "claude-opus-5-high", "claude-sonnet-5-thinking", "claude-sonnet-5-thinking"]);
        assert_eq!(borrowed, [(0, "claude-opus-5-high".to_string()), (3, "claude-sonnet-5-thinking".to_string())], "Fable borrows the next cheaper slot, Haiku the nearest above");

        // A group with no Claude name at all still answers every alias.
        let (slots, borrowed) = slot_names(&group(&["gpt-5.6-sol", "gpt-6-astra"]), &catalog(&["codex/gpt-5.6-sol"]), &[None, None, None, None]);
        assert_eq!(slots, ["gpt-5.6-sol"; 4]);
        assert_eq!(borrowed.len(), 4);
    }

    #[test]
    fn models_new_on_the_account_are_added_and_nothing_the_group_holds_is_touched() {
        // Newest first, as the catalog lists them; the group was written before 5-2 existed.
        let cat = catalog(&["claude-code/claude-fable-5-2", "claude-code/claude-fable-5-1", "claude-code/claude-opus-5", "codex/gpt-5.6-sol", "claude-code/claude-fable-5-2"]);
        let g = group(&["my-alias", "claude-fable-5-1"]);
        let (new, left_out) = new_models(AgentKind::ClaudeCode, &g, &cat);
        let names: Vec<&str> = new.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["claude-fable-5-2", "claude-opus-5"], "only the agent's own provider, only what no name serves yet, each once");
        assert_eq!(new[0].targets, [GroupTarget { model: "claude-code/claude-fable-5-2".into(), account_id: None }], "answering as itself");
        assert!(left_out.is_empty());
        // The Fable slot then follows the account's newest model of the family.
        let all = Group { models: g.models.iter().cloned().chain(new).collect(), ..g };
        assert_eq!(slot_names(&all, &cat, &[None, None, None, None]).0[0], "claude-fable-5-2");

        // A full group gains nothing, and says what it has no room for.
        let full = group(&(0..MAX_GROUP_MODELS).map(|i| format!("m{i}")).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>());
        let (new, left_out) = new_models(AgentKind::ClaudeCode, &full, &cat);
        assert!(new.is_empty());
        assert_eq!(left_out, ["claude-fable-5-2", "claude-fable-5-1", "claude-opus-5"]);
        // Nothing new: nothing to add.
        assert!(new_models(AgentKind::ClaudeCode, &group(&["claude-fable-5-2", "claude-fable-5-1", "claude-opus-5"]), &cat).0.is_empty());
    }

    #[test]
    fn the_settings_file_holds_setups_keys_and_clears_what_would_leak_in() {
        let slots = strings(&["claude-fable-5-1", "claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"]);
        let settings = claude_settings("https://p.example", &slots, None, "sk-kano-proxy-abc");
        assert_eq!(settings["disableClaudeAiConnectors"], true);
        assert_eq!(settings["disableAgentView"], true, "a backgrounded session would outlive its key");
        assert!(settings.get("model").is_none(), "the user's saved model is left alone when it can work");
        let env = &settings["env"];
        assert_eq!(env["ANTHROPIC_BASE_URL"], "https://p.example/g/claude-code/anthropic");
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "sk-kano-proxy-abc");
        assert_eq!(env["CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"], "1");
        assert_eq!(env["ANTHROPIC_DEFAULT_FABLE_MODEL"], "claude-fable-5-1");
        assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "claude-haiku-4-5");
        // A --settings file only overrides the keys it names: another credential, another
        // gateway's headers, a provider switch or a model pin must be named to stay out.
        // Spelled out here, not read from the constant: dropping a name must fail a test.
        let cleared = [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_CUSTOM_HEADERS",
            "CLAUDE_CODE_EXTRA_BODY",
            "ANTHROPIC_MODEL",
            "ANTHROPIC_DEFAULT_MODEL",
            "ANTHROPIC_SMALL_FAST_MODEL",
            "CLAUDE_CODE_SUBAGENT_MODEL",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
            "CLAUDE_CODE_USE_MANTLE",
            "CLAUDE_CODE_USE_ANTHROPIC_AWS",
        ];
        for name in cleared {
            assert_eq!(env[name], "", "{name}");
        }
        assert_eq!(env.as_object().unwrap().len(), 3 + 4 + cleared.len(), "endpoint, key, discovery; four slots; and exactly these cleared");

        // The model the user saved: an alias or a served name stays, another gateway's id
        // is replaced by the Opus slot for this session.
        let g = group(&["claude-opus-5", "claude-sonnet-5"]);
        assert_eq!(start_model(None, &g, &slots), None);
        assert_eq!(start_model(Some("opus[1m]"), &g, &slots), None);
        assert_eq!(start_model(Some("default"), &g, &slots), None);
        assert_eq!(start_model(Some("claude-sonnet-5"), &g, &slots), None);
        assert_eq!(start_model(Some("us.anthropic.claude-opus-5-v1:0"), &g, &slots).as_deref(), Some("claude-opus-5"));
        assert_eq!(claude_settings("https://p.example", &slots, Some("claude-opus-5"), "k")["model"], "claude-opus-5");
    }

    #[test]
    fn codex_overrides_never_carry_the_key_and_quote_only_what_codex_would_misread() {
        let args = codex_overrides("https://p.example", "gpt-5.6-sol", Some("xhigh"), Some(Path::new("/tmp/launch/x.models.json")));
        let pairs: Vec<&str> = args
            .chunks(2)
            .map(|c| {
                assert_eq!(c[0], "-c");
                c[1].as_str()
            })
            .collect();
        assert_eq!(
            pairs,
            [
                "model_provider=kano-launch",
                "model_providers.kano-launch.name=Kano Proxy",
                "model_providers.kano-launch.base_url=https://p.example/g/codex-cli/openai/v1",
                "model_providers.kano-launch.wire_api=responses",
                "model_providers.kano-launch.env_key=KANO_PROXY_API_KEY",
                "model=gpt-5.6-sol",
                "model_reasoning_effort=xhigh",
                "model_catalog_json=/tmp/launch/x.models.json",
            ]
        );
        assert!(!args.iter().any(|a| a.contains("bearer") || a.contains("sk-kano-proxy")), "the key travels in the environment");
        assert_eq!(codex_overrides("https://p.example", "m", None, None).len(), 12, "no effort, no catalog: neither override");
        // Codex reads the value as TOML and falls back to the literal string: only a value
        // that *is* TOML needs quoting to stay a string.
        assert_eq!(toml_arg("gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(toml_arg(r"C:\Users\me\AppData\Roaming\kano-proxy\launch\x.models.json"), r"C:\Users\me\AppData\Roaming\kano-proxy\launch\x.models.json");
        assert_eq!(toml_arg("true"), "\"true\"");
        assert_eq!(toml_arg("2026"), "\"2026\"");
        assert_eq!(toml_arg("'quoted'"), "\"'quoted'\"");
    }

    #[test]
    fn arguments_that_would_fight_the_launch_are_refused_before_anything_is_minted() {
        let args = |model: Option<&str>, effort: Option<&str>, rest: &[&str]| LaunchArgs {
            dry_run: false,
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
            args: rest.iter().map(OsString::from).collect(),
        };
        assert!(check_args(AgentKind::ClaudeCode, &args(None, None, &["-p", "hi", "--model", "opus"])).is_ok());
        assert!(check_args(AgentKind::ClaudeCode, &args(None, None, &["--settings", "x.json"])).is_err());
        assert!(check_args(AgentKind::ClaudeCode, &args(None, None, &["--settings={}"])).is_err());
        assert!(check_args(AgentKind::ClaudeCode, &args(None, None, &["--bg", "fix it"])).is_err(), "the key does not outlive the command");
        assert!(check_args(AgentKind::ClaudeCode, &args(None, None, &["--settings-sources", "user"])).is_ok(), "only the flag itself");
        assert!(check_args(AgentKind::ClaudeCode, &args(Some("claude-opus-5"), None, &[])).unwrap_err().to_string().contains("-- --model"));
        assert!(check_args(AgentKind::Codex, &args(Some("gpt-5.6-sol"), Some("high"), &["exec", "x"])).is_ok());
        assert!(check_args(AgentKind::Codex, &args(None, Some("minimal"), &[])).is_err());
        assert!(check_args(AgentKind::Codex, &args(None, None, &["--oss"])).is_err(), "off the proxy");

        let g = Group { slug: "codex-cli".into(), ..group(&["gpt-5.6-mini", "gpt-5.6-sol", "gpt-6-astra"]) };
        let cat = catalog(&["codex/gpt-5.6-sol", "codex/gpt-5.6-mini"]);
        assert_eq!(codex_model(&g, &cat, None, None).unwrap(), "gpt-5.6-sol", "the account's first codex model, not whatever the group lists first");
        assert_eq!(codex_model(&g, &cat, None, Some("gpt-6-astra")).unwrap(), "gpt-6-astra", "what setup wrote on this machine");
        assert_eq!(codex_model(&g, &cat, None, Some("o3")).unwrap(), "gpt-5.6-sol", "unless the group no longer serves it");
        assert_eq!(codex_model(&g, &cat, Some("gpt-6-astra"), Some("gpt-5.6-mini")).unwrap(), "gpt-6-astra");
        assert!(codex_model(&g, &cat, Some("o3"), None).unwrap_err().to_string().contains("gpt-5.6-mini, gpt-5.6-sol, gpt-6-astra"));
        assert_eq!(codex_model(&g, &[], None, None).unwrap(), "gpt-5.6-mini", "nothing else to go by: the group's first");
    }

    #[cfg(unix)]
    mod end_to_end {
        use super::super::*;
        use crate::agents::tests::{mock_server, HomeGuard, PathGuard, Sandbox};
        use std::os::unix::fs::PermissionsExt;

        const SECRET: &str = "sk-kano-proxy-e2e-secret";

        fn program(sandbox: &Sandbox, name: &str, script: &str) -> (PathBuf, PathBuf) {
            let bin = sandbox.dir.join("bin");
            let out = sandbox.dir.join("seen");
            fs::create_dir_all(&bin).unwrap();
            fs::create_dir_all(&out).unwrap();
            let path = bin.join(name);
            fs::write(&path, script.replace("@OUT@", &out.display().to_string())).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            (bin, out)
        }

        /// A stand-in agent on a directory of its own: records its arguments, its
        /// environment and the files the launch handed it, then exits with `code`.
        fn stand_in(sandbox: &Sandbox, name: &str, code: i32) -> (PathBuf, PathBuf) {
            let script = r#"#!/bin/sh
if [ "$1" = debug ]; then
  echo '{"models":[{"slug":"gpt-5.6-sol","visibility":"list","supported_reasoning_levels":[{"effort":"minimal"},{"effort":"high"}],"default_reasoning_level":"minimal"},{"slug":"gpt-other","visibility":"list","supported_reasoning_levels":[],"default_reasoning_level":"high"}]}'
  exit 0
fi
out='@OUT@'
mode() { ls -ld "$1" | cut -c1-10; }
: > "$out/argv"
prev=""
for a in "$@"; do
  printf '%s\n' "$a" >> "$out/argv"
  if [ "$prev" = "--settings" ]; then
    cp "$a" "$out/settings.json"; mode "$a" > "$out/settings.mode"; mode "$(dirname "$a")" > "$out/dir.mode"
    for lock in "$(dirname "$a")"/*.lock; do mode "$lock" > "$out/lock.mode"; done
  fi
  case "$a" in model_catalog_json=*) cp "${a#model_catalog_json=}" "$out/models.json"; mode "${a#model_catalog_json=}" > "$out/models.mode";; esac
  prev="$a"
done
env > "$out/env"
exit @CODE@
"#;
            program(sandbox, name, &script.replace("@CODE@", &code.to_string()))
        }

        fn launch_args(rest: &[&str]) -> LaunchArgs {
            LaunchArgs { dry_run: false, model: None, effort: None, args: rest.iter().map(OsString::from).collect() }
        }

        fn lines(path: &Path) -> Vec<String> {
            fs::read_to_string(path).unwrap().lines().map(str::to_string).collect()
        }

        fn left_in(dir: &Path) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
            names.sort();
            names
        }

        fn record(base: &str, device: &str, key_id: Option<&str>) -> String {
            serde_json::to_string(&RecordBody { base_url: base.into(), device_id: device.into(), key_name: "claude-code-launch@my-mac".into(), key_id: key_id.map(str::to_string) }).unwrap()
        }

        fn count(seen: &[(String, String, String)], method: &str, path_prefix: &str) -> usize {
            seen.iter().filter(|(m, p, _)| m == method && p.starts_with(path_prefix)).count()
        }

        /// An environment variable for the length of a test; `HomeGuard` must be held.
        struct EnvGuard(&'static str);

        impl EnvGuard {
            fn set(name: &'static str, value: &Path) -> Self {
                std::env::set_var(name, value);
                Self(name)
            }
        }

        impl Drop for EnvGuard {
            fn drop(&mut self) {
                std::env::remove_var(self.0);
            }
        }

        /// Claude Code, launched against a group that exists: the agent gets a settings file
        /// of the launch's own and its exit code comes back; the user's settings.json, the
        /// group and the state file are exactly as they were; the key is revoked — with a
        /// token refreshed for it — and the launch's files are gone.
        #[tokio::test(flavor = "multi_thread")]
        async fn claude_code_runs_on_a_key_of_its_own_and_leaves_nothing_behind() {
            let server = mock_server(true);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let (bin, out) = stand_in(&sandbox, "claude", 7);
            let _path = PathGuard::prepend(&bin);
            let own = sandbox.dir.join(".claude").join("settings.json");
            fs::create_dir_all(own.parent().unwrap()).unwrap();
            let before = "{\n  \"model\": \"another-gateway/opus\",\n  \"env\": {\"ANTHROPIC_BASE_URL\": \"https://other.example\", \"ANTHROPIC_MODEL\": \"pinned\", \"ANTHROPIC_DEFAULT_OPUS_MODEL\": \"theirs\"}\n}\n";
            fs::write(&own, before).unwrap();
            let state_before = fs::read_to_string(&sandbox.file.path).unwrap();

            let exit = run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&["-p", "hi there"])).await.unwrap();
            assert_eq!(exit, Exit::Code(7), "the agent's own exit code");

            let argv = lines(&out.join("argv"));
            assert_eq!(argv[0], "--settings");
            assert!(Path::new(&argv[1]).starts_with(launch_dir(&sandbox.file.path)), "{argv:?}");
            assert_eq!(&argv[2..], ["-p", "hi there"], "agent args untouched, after the launch's own");
            assert!(!argv.iter().any(|a| a.contains(SECRET)), "the key is never on the command line");
            assert!(!fs::read_to_string(out.join("env")).unwrap().contains(SECRET), "nor in the environment the launch gives Claude Code — the file carries it");
            for file in ["settings.mode", "lock.mode"] {
                assert_eq!(fs::read_to_string(out.join(file)).unwrap().trim(), "-rw-------", "{file}");
            }
            assert_eq!(fs::read_to_string(out.join("dir.mode")).unwrap().trim(), "drwx------");
            let seen: Value = serde_json::from_str(&fs::read_to_string(out.join("settings.json")).unwrap()).unwrap();
            assert_eq!(seen["env"]["ANTHROPIC_BASE_URL"], format!("{}/g/claude-code/anthropic", server.base));
            assert_eq!(seen["env"]["ANTHROPIC_AUTH_TOKEN"], SECRET);
            assert_eq!(seen["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "claude-opus-5", "the group's name for the slot, not another gateway's");
            assert_eq!(seen["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "claude-haiku-4-5", "a model the account gained since the group was written is served too");
            assert_eq!(seen["env"]["ANTHROPIC_MODEL"], "", "the user's pin cannot reach a launched session");
            assert_eq!(seen["model"], "claude-opus-5", "nor a saved model this group does not serve");

            assert_eq!(fs::read_to_string(&own).unwrap(), before, "the user's settings.json is byte for byte what it was");
            let state_after = fs::read_to_string(&sandbox.file.path).unwrap();
            assert!(!state_after.contains(SECRET));
            let (a, b): (Value, Value) = (serde_json::from_str(&state_before).unwrap(), serde_json::from_str(&state_after).unwrap());
            assert_eq!((&a["agents"], &a["providers"], &a["device_id"]), (&b["agents"], &b["providers"], &b["device_id"]), "a launch is not a setup: no record");
            assert!(left_in(&launch_dir(&sandbox.file.path)).is_empty(), "the launch's files are gone");

            let seen = server.requests.lock().unwrap();
            let minted = seen.iter().find(|(m, p, _)| m == "POST" && p == "/cli/v1/keys").expect("a key for this launch");
            assert_eq!(serde_json::from_str::<Value>(&minted.2).unwrap()["name"], "claude-code-launch@my-mac");
            assert_eq!(count(&seen, "DELETE", "/cli/v1/keys/key_"), 1, "revoked when the agent exited: {seen:?}");
            assert_eq!(count(&seen, "POST", "/cli/v1/token"), 2, "a token for the start and a fresh one for the revoke — a session can outlive the first");
            // The group: what it held is sent back exactly as it was, with the models the
            // account gained since appended — never created anew, never rewritten.
            assert_eq!(count(&seen, "POST", "/cli/v1/model-groups"), 0);
            let updated = seen.iter().find(|(m, p, _)| m == "PUT" && p == "/cli/v1/model-groups/mgrp_1").expect("new models are added");
            let models = serde_json::from_str::<Value>(&updated.2).unwrap()["models"].clone();
            assert_eq!(
                models.as_array().unwrap()[..2],
                [
                    json!({ "name": "claude-opus-5", "targets": [{ "model": "grok/grok-4.5" }] }),
                    json!({ "name": "my-alias", "targets": [{ "model": "codex/gpt-5.6-sol" }] })
                ],
                "the user's own names and targets are untouched"
            );
            let added: Vec<&str> = models.as_array().unwrap()[2..].iter().map(|m| m["name"].as_str().unwrap()).collect();
            assert_eq!(added, ["claude-sonnet-5", "claude-haiku-4-5", "claude-fable-5-1"], "each answering as itself, in catalog order");
            assert_eq!(models[2]["targets"], json!([{ "model": "claude-code/claude-sonnet-5" }]));
        }

        /// A machine `agent setup` already wrote: the names setup gave the slots are the
        /// ones a launch uses, and a saved model the group serves is left alone. The user's
        /// settings are read from the directory Claude Code itself will read.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_launch_on_a_set_up_machine_keeps_the_names_setup_wrote() {
            let server = mock_server(true);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let (bin, out) = stand_in(&sandbox, "claude", 0);
            let _path = PathGuard::prepend(&bin);
            // What ~ holds is not what Claude Code will read here…
            fs::create_dir_all(sandbox.dir.join(".claude")).unwrap();
            fs::write(sandbox.dir.join(".claude").join("settings.json"), "{\"model\": \"another-gateway/opus\"}").unwrap();
            // …this is.
            let config = sandbox.dir.join("claude-config");
            fs::create_dir_all(&config).unwrap();
            let own = format!(
                "{{\"model\": \"my-alias\", \"env\": {{\"ANTHROPIC_BASE_URL\": \"{}/g/claude-code/anthropic\", \"ANTHROPIC_DEFAULT_SONNET_MODEL\": \"my-alias\", \"ANTHROPIC_DEFAULT_HAIKU_MODEL\": \"dropped-since\"}}}}",
                server.base
            );
            fs::write(config.join("settings.json"), &own).unwrap();
            let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", &config);

            assert_eq!(run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&[])).await.unwrap(), Exit::Code(0));
            let seen: Value = serde_json::from_str(&fs::read_to_string(out.join("settings.json")).unwrap()).unwrap();
            assert_eq!(seen["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"], "my-alias", "the name setup wrote for the slot");
            assert_eq!(seen["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "claude-opus-5");
            assert_eq!(seen["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "claude-haiku-4-5", "a name the group no longer serves is not used: the account's own model of the family is");
            assert!(seen.get("model").is_none(), "a saved model the group serves is the user's choice");
            assert_eq!(fs::read_to_string(config.join("settings.json")).unwrap(), own);
        }

        /// Codex on an account with no group yet: the group is created once with setup's
        /// defaults, the configuration is all `-c` overrides, the key is in the environment
        /// and nowhere on the command line, the catalog is the launch's own file, and
        /// config.toml is untouched — though an effort in it the proxy would refuse is not
        /// left to fail every turn.
        #[tokio::test(flavor = "multi_thread")]
        async fn codex_is_configured_by_overrides_and_reads_its_key_from_the_environment() {
            let server = mock_server(false);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let (bin, out) = stand_in(&sandbox, "codex", 0);
            let _path = PathGuard::prepend(&bin);
            let own = sandbox.dir.join(".codex").join("config.toml");
            fs::create_dir_all(own.parent().unwrap()).unwrap();
            let before = "model = \"o3\"\nmodel_provider = \"kano\"\nmodel_reasoning_effort = \"minimal\"\n\n[model_providers.kano]\nexperimental_bearer_token = \"sk-kano-proxy-from-setup\"\n";
            fs::write(&own, before).unwrap();

            assert_eq!(run(&sandbox.file, AgentKind::Codex, launch_args(&["exec", "run the tests"])).await.unwrap(), Exit::Code(0));

            let argv = lines(&out.join("argv"));
            let has = |v: &str| argv.iter().any(|a| a == v);
            assert!(has("model_provider=kano-launch") && has("model_providers.kano-launch.env_key=KANO_PROXY_API_KEY"), "{argv:?}");
            assert!(has(&format!("model_providers.kano-launch.base_url={}/g/codex-cli/openai/v1", server.base)));
            assert!(has("model=gpt-5.6-sol"));
            assert!(has("model_reasoning_effort=high"), "the config's `minimal` is a 400 at the proxy: {argv:?}");
            assert_eq!(&argv[argv.len() - 2..], ["exec", "run the tests"], "agent args last, so the user's own -c wins");
            assert!(!argv.iter().any(|a| a.contains(SECRET)), "the key is never on the command line");
            assert!(lines(&out.join("env")).contains(&format!("KANO_PROXY_API_KEY={SECRET}")));
            let catalog_arg = argv.iter().find_map(|a| a.strip_prefix("model_catalog_json=")).expect("a catalog of the launch's own");
            assert!(Path::new(catalog_arg).is_absolute(), "Codex reads a relative path against its own directory: {catalog_arg}");
            assert_eq!(fs::read_to_string(out.join("models.mode")).unwrap().trim(), "-rw-------");
            let catalog: Value = serde_json::from_str(&fs::read_to_string(out.join("models.json")).unwrap()).unwrap();
            assert_eq!(catalog["models"].as_array().unwrap().len(), 1, "only what the group serves");
            assert_eq!(catalog["models"][0]["supported_reasoning_levels"], json!([{ "effort": "high" }]), "efforts the proxy accepts");

            assert_eq!(fs::read_to_string(&own).unwrap(), before, "config.toml is byte for byte what it was");
            assert!(sandbox.file.load().unwrap().agents.is_empty());
            assert!(left_in(&launch_dir(&sandbox.file.path)).is_empty());
            let seen = server.requests.lock().unwrap();
            let created = seen.iter().find(|(m, p, _)| m == "POST" && p == "/cli/v1/model-groups").expect("no group yet: created once");
            let body: Value = serde_json::from_str(&created.2).unwrap();
            assert_eq!((&body["slug"], &body["models"][0]["name"], &body["models"][0]["targets"][0]["model"]), (&json!("codex-cli"), &json!("gpt-5.6-sol"), &json!("codex/gpt-5.6-sol")));
            assert_eq!(count(&seen, "DELETE", "/cli/v1/keys/key_"), 1);
        }

        /// Two first launches at once: the one that loses the race to create the group uses
        /// the group the other created.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_launch_that_loses_the_race_to_create_the_group_uses_the_one_that_won() {
            let server = mock_server(false);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let (bin, out) = stand_in(&sandbox, "claude", 0);
            let _path = PathGuard::prepend(&bin);
            *server.lose_group_race.lock().unwrap() = true;

            assert_eq!(run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&[])).await.unwrap(), Exit::Code(0));
            let seen: Value = serde_json::from_str(&fs::read_to_string(out.join("settings.json")).unwrap()).unwrap();
            assert_eq!(seen["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "claude-opus-5", "a name from the group that exists now");
            assert_eq!(count(&server.requests.lock().unwrap(), "POST", "/cli/v1/model-groups"), 1, "one attempt, then the list");
        }

        /// A launch that died left a record nobody holds: the next launch — the same
        /// sign-in — revokes its key and deletes its files, plaintext included. A record a
        /// living launch holds is left alone, and so is one that has only just been
        /// created. A record of an earlier sign-in is not revoked (this device could not),
        /// only cleared away — whichever of server and device differs.
        #[tokio::test(flavor = "multi_thread")]
        async fn the_next_launch_revokes_what_a_dead_one_left_and_leaves_the_rest_alone() {
            let server = mock_server(true);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let (bin, _out) = stand_in(&sandbox, "claude", 0);
            let _path = PathGuard::prepend(&bin);
            let dir = launch_dir(&sandbox.file.path);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("dead.lock"), record(&server.base, "clidev_1", Some("key_orphan"))).unwrap();
            fs::write(dir.join("dead.settings.json"), "{\"env\":{\"ANTHROPIC_AUTH_TOKEN\":\"sk-kano-proxy-left-behind\"}}").unwrap();
            fs::write(dir.join("live.lock"), record(&server.base, "clidev_1", Some("key_live"))).unwrap();
            fs::write(dir.join("live.settings.json"), "{}").unwrap();
            fs::write(dir.join("starting.lock"), record(&server.base, "clidev_1", None)).unwrap();
            fs::write(dir.join("unminted.lock"), record(&server.base, "clidev_1", None)).unwrap();
            let long_ago = SystemTime::now() - Duration::from_secs(600);
            fs::File::options().write(true).open(dir.join("unminted.lock")).unwrap().set_modified(long_ago).unwrap();
            fs::write(dir.join("other-server.lock"), record("https://other.example", "clidev_1", Some("key_other_server"))).unwrap();
            fs::write(dir.join("other-device.lock"), record(&server.base, "clidev_9", Some("key_other_device"))).unwrap();
            fs::write(dir.join("planted.lock"), record(&server.base, "clidev_1", Some("../device"))).unwrap();
            fs::File::options().write(true).open(dir.join("planted.lock")).unwrap().set_modified(long_ago).unwrap();
            let live = fs::File::open(dir.join("live.lock")).unwrap();
            live.lock().unwrap();
            assert_eq!(counts(&sandbox.file.path), (1, 5), "one running; the one just created is neither");

            assert_eq!(run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&[])).await.unwrap(), Exit::Code(0));
            {
                let seen = server.requests.lock().unwrap();
                let revoked: Vec<&str> = seen.iter().filter(|(m, _, _)| m == "DELETE").map(|(_, p, _)| p.as_str()).collect();
                assert_eq!(revoked.len(), 2, "the dead launch's key and this launch's own: {revoked:?}");
                assert!(revoked.contains(&"/cli/v1/keys/key_orphan"), "{revoked:?}");
            }
            assert_eq!(left_in(&dir), ["live.lock", "live.settings.json", "starting.lock"], "the dead launch's key file went with its record");
            assert_eq!(counts(&sandbox.file.path), (1, 0));
        }

        /// The server cannot be reached when the agent exits — or answers `401`, which on
        /// a delete proves nothing about the key: the plaintext file still goes, the
        /// record stays for the next launch, and the agent's exit code is kept.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_key_that_cannot_be_revoked_keeps_its_record_for_the_next_launch() {
            for status in [503, 401] {
                let server = mock_server(true);
                let sandbox = Sandbox::new(&server.base);
                let _home = HomeGuard::set(&sandbox.dir);
                let (bin, _out) = stand_in(&sandbox, "claude", 3);
                let _path = PathGuard::prepend(&bin);
                *server.revoke_status.lock().unwrap() = Some(status);

                assert_eq!(run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&[])).await.unwrap(), Exit::Code(3));
                let dir = launch_dir(&sandbox.file.path);
                let left = left_in(&dir);
                assert_eq!(left.len(), 1, "{status}: only the record: {left:?}");
                let text = fs::read_to_string(dir.join(&left[0])).unwrap();
                assert!(!text.contains(SECRET), "a record never holds the plaintext");
                let key_id = serde_json::from_str::<RecordBody>(&text).unwrap().key_id.expect("the id the next launch needs");

                *server.revoke_status.lock().unwrap() = None;
                assert_eq!(run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&[])).await.unwrap(), Exit::Code(3));
                assert!(left_in(&dir).is_empty());
                assert_eq!(count(&server.requests.lock().unwrap(), "DELETE", &format!("/cli/v1/keys/{key_id}")), 2, "{status}: refused once, revoked by the next launch");
            }
        }

        /// The server refuses to mint: nothing was created, so nothing is left — no record
        /// to sweep, no agent started.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_key_the_server_refuses_to_mint_leaves_nothing() {
            let server = mock_server(true);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let (bin, out) = stand_in(&sandbox, "claude", 0);
            let _path = PathGuard::prepend(&bin);
            *server.refuse_mint.lock().unwrap() = true;

            assert!(run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&[])).await.is_err());
            assert!(!out.join("argv").exists(), "the agent was not started");
            assert!(left_in(&launch_dir(&sandbox.file.path)).is_empty());
            assert_eq!(count(&server.requests.lock().unwrap(), "DELETE", "/cli/v1/keys/"), 0, "no key, nothing to revoke");
        }

        /// The agent cannot be started (here: an interpreter that does not exist): the key
        /// minted a moment earlier is revoked and nothing is left behind.
        #[tokio::test(flavor = "multi_thread")]
        async fn an_agent_that_cannot_start_does_not_keep_its_key() {
            let server = mock_server(true);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let (bin, out) = program(&sandbox, "claude", "#!/nonexistent/kano-proxy-test-interpreter\n");
            let _path = PathGuard::prepend(&bin);

            let error = run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&[])).await.unwrap_err();
            assert!(format!("{error:#}").contains("cannot start Claude Code"), "{error:#}");
            assert!(!format!("{error:#}").contains(SECRET));
            assert!(!out.join("argv").exists());
            assert!(left_in(&launch_dir(&sandbox.file.path)).is_empty(), "no record, no plaintext file");
            let seen = server.requests.lock().unwrap();
            assert_eq!(count(&seen, "POST", "/cli/v1/keys"), 1);
            assert_eq!(count(&seen, "DELETE", "/cli/v1/keys/key_"), 1, "{seen:?}");
        }

        /// A stop signal while the launch is still minting: it is not killed with a key
        /// nobody will revoke — the mint completes, the key is revoked, the agent never
        /// starts, and the launch leaves by the signal.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_signal_before_the_agent_starts_cancels_the_launch_and_revokes_the_key() {
            let server = mock_server(true);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let (bin, out) = stand_in(&sandbox, "claude", 0);
            let _path = PathGuard::prepend(&bin);
            *server.signal_on_mint.lock().unwrap() = Some(libc::SIGTERM);

            assert_eq!(run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&[])).await.unwrap(), Exit::Signal(libc::SIGTERM));
            assert!(!out.join("argv").exists(), "the agent was not started");
            assert!(left_in(&launch_dir(&sandbox.file.path)).is_empty());
            let seen = server.requests.lock().unwrap();
            assert_eq!(count(&seen, "DELETE", "/cli/v1/keys/key_"), 1, "{seen:?}");
        }

        /// Told to stop while the agent runs (`kill`, a service manager — the agent itself
        /// was not signalled): the launch passes the signal on, the agent gets to exit on
        /// its own, the key is revoked, and the launch leaves by that signal.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_stop_signal_is_passed_on_to_the_agent_before_the_key_is_revoked() {
            let server = mock_server(true);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let script = r#"#!/bin/sh
out='@OUT@'
trap 'echo stopped > "$out/stopped"; exit 0' TERM
echo started > "$out/started"
i=0
while [ $i -lt 300 ]; do sleep 0.1; i=$((i + 1)); done
"#;
            let (bin, out) = program(&sandbox, "claude", script);
            let _path = PathGuard::prepend(&bin);
            let started = out.join("started");
            let stopper = std::thread::spawn(move || {
                // Only once the agent runs: the launch's handlers are in place by then.
                for _ in 0..500 {
                    if started.exists() {
                        // SAFETY: raising a signal on this process, which handles it.
                        unsafe {
                            libc::raise(libc::SIGTERM);
                        }
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });

            assert_eq!(run(&sandbox.file, AgentKind::ClaudeCode, launch_args(&[])).await.unwrap(), Exit::Signal(libc::SIGTERM));
            stopper.join().unwrap();
            assert!(out.join("stopped").exists(), "the agent heard the signal and exited on its own");
            assert!(left_in(&launch_dir(&sandbox.file.path)).is_empty());
            assert_eq!(count(&server.requests.lock().unwrap(), "DELETE", "/cli/v1/keys/key_"), 1);
        }

        /// `kano-proxy logout`: the device is revoked on the server and its keys with it,
        /// so this sign-in's dead records are deleted without a revoke of their own.
        #[tokio::test(flavor = "multi_thread")]
        async fn logout_clears_the_records_of_dead_launches_once_the_device_is_gone() {
            let server = mock_server(true);
            let sandbox = Sandbox::new(&server.base);
            let _home = HomeGuard::set(&sandbox.dir);
            let dir = launch_dir(&sandbox.file.path);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("dead.lock"), record(&server.base, "clidev_1", Some("key_orphan"))).unwrap();
            fs::write(dir.join("dead.settings.json"), "{}").unwrap();

            crate::commands::cmd_logout(&sandbox.file, true, false, false).await.unwrap();
            assert!(left_in(&dir).is_empty());
            let seen = server.requests.lock().unwrap();
            assert_eq!(count(&seen, "DELETE", "/cli/v1/device"), 1);
            assert_eq!(count(&seen, "DELETE", "/cli/v1/keys/"), 0, "the device took its keys with it");
        }

        /// `--dry-run` reads and prints; it creates, mints, revokes and starts nothing.
        #[tokio::test(flavor = "multi_thread")]
        async fn a_dry_run_creates_mints_revokes_and_starts_nothing() {
            // With no group (one would be created) and with one that would gain models.
            for group_exists in [false, true] {
                let server = mock_server(group_exists);
                let sandbox = Sandbox::new(&server.base);
                let _home = HomeGuard::set(&sandbox.dir);
                let (bin, out) = stand_in(&sandbox, "claude", 0);
                let _path = PathGuard::prepend(&bin);
                let dir = launch_dir(&sandbox.file.path);
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("dead.lock"), record(&server.base, "clidev_1", Some("key_orphan"))).unwrap();
                let args = LaunchArgs { dry_run: true, ..launch_args(&[]) };
                assert_eq!(run(&sandbox.file, AgentKind::ClaudeCode, args).await.unwrap(), Exit::Code(0));
                let seen = server.requests.lock().unwrap();
                assert!(seen.iter().all(|(m, p, _)| m == "GET" || p == "/cli/v1/token"), "{seen:?}");
                assert!(!out.join("argv").exists(), "the agent was not started");
                assert_eq!(left_in(&dir), ["dead.lock"], "not even the sweep");
            }
        }
    }
}
