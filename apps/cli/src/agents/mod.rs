//! `kano-proxy agent setup|restore|list` (docs/cli.md § Command surface):
//! point a coding agent on this machine at the proxy — through a model group
//! that serves the agent's real model ids and an API key this device mints —
//! and undo it exactly. The agent-specific knowledge (which file, which
//! fields, how to pick models) lives in `claude_code` and `codex`; this
//! module owns the flow both share: plan → group → key → config write →
//! verify → record, and the reverse.

pub mod claude_code;
pub mod codex;
mod doc;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::api::{self, CatalogModel, Group, GroupModel, GroupTarget};
use crate::state::{backups_dir, AgentState, FieldRecord, StateFile};
use crate::tui::{self, Nav};
use crate::tunnel::TokenCache;

use doc::{Doc, Kind};

/// The placeholder a plan carries where the key goes; replaced after minting, never recorded.
pub const KEY_PLACEHOLDER: &str = "<new key>";
/// What the state file records as the key field's written value — never the plaintext.
const KEY_RECORD: &str = "<key>";
/// Reasoning efforts the proxy accepts (docs/api.md § Reasoning); `minimal`/`ultra` are a 400.
pub const EFFORTS: [&str; 4] = ["low", "medium", "high", "xhigh"];
/// Models per group, the server's cap (docs/providers.md § Model groups).
pub const MAX_GROUP_MODELS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AgentKind {
    /// Claude Code — ~/.claude/settings.json
    #[value(name = "claude-code")]
    ClaudeCode,
    /// Codex CLI — ~/.codex/config.toml
    #[value(name = "codex")]
    Codex,
}

impl AgentKind {
    pub const ALL: [AgentKind; 2] = [AgentKind::ClaudeCode, AgentKind::Codex];

    pub fn id(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Codex => "codex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex CLI",
        }
    }

    /// The provider whose ids the agent natively understands.
    pub fn provider(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Codex => "codex",
        }
    }

    pub fn group_slug(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Codex => "codex-cli",
        }
    }

    pub fn group_name(self) -> &'static str {
        self.label()
    }

    /// The base the agent is pointed at: the group's endpoint in the agent's wire shape.
    pub fn group_base(self, base_url: &str) -> String {
        match self {
            AgentKind::ClaudeCode => format!("{base_url}/g/{}/anthropic", self.group_slug()),
            AgentKind::Codex => format!("{base_url}/g/{}/openai/v1", self.group_slug()),
        }
    }

    fn doc_kind(self) -> Kind {
        match self {
            AgentKind::ClaudeCode => Kind::Json,
            AgentKind::Codex => Kind::Toml,
        }
    }

    pub fn config_path(self) -> PathBuf {
        match self {
            AgentKind::ClaudeCode => home_dir().join(".claude").join("settings.json"),
            AgentKind::Codex => home_dir().join(".codex").join("config.toml"),
        }
    }

    /// The field that carries the key — its recorded value is redacted.
    fn key_field(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => claude_code::KEY_FIELD,
            AgentKind::Codex => codex::KEY_FIELD,
        }
    }
}

/// `~` on macOS/Linux, `%USERPROFILE%` on Windows — where both agents keep their config.
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(not(windows))]
    let var = "HOME";
    std::env::var_os(var).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| PathBuf::from("."))
}

// ---------------------------------------------------------------------------
// plans

/// What is in place before `setup` runs — the agent's config file as it is now and the group
/// on the server — so the pickers start from the current choices and a rerun is an edit, not
/// a fresh start (docs/cli.md § Command surface).
pub struct Current<'a> {
    pub doc: Option<&'a Doc>,
    pub group: Option<&'a Group>,
    /// The agent is set up against this proxy now: the group exists and the config points
    /// at its endpoint. When false there is nothing to compare a draft against — saving
    /// would change everything.
    pub configured: bool,
}

impl Current<'_> {
    /// A string field of the current config, when present.
    pub fn field(&self, field: &str) -> Option<String> {
        self.doc.and_then(|d| d.get(field)).and_then(|v| v.as_str().map(str::to_string))
    }

    /// The targets the group already holds for `name`, in order; `None` when it has no such model.
    pub fn targets(&self, name: &str) -> Option<Vec<String>> {
        self.group
            .and_then(|g| g.models.iter().find(|m| m.name == name))
            .map(|m| m.targets.iter().map(|t| t.model.clone()).collect())
    }
}

/// What `setup` will do, computed before anything is written — printed as-is by `--dry-run`.
#[derive(Debug)]
pub struct Plan {
    pub group_models: Vec<GroupModel>,
    /// Field path → value; the key field holds `KEY_PLACEHOLDER` until the key exists.
    pub fields: Vec<(String, Value)>,
    pub summary: Vec<String>,
    /// Codex only: the catalog the CLI will generate, when a `codex` binary was found.
    pub catalog: Option<codex::CatalogPlan>,
}

pub struct SetupArgs {
    pub no_tui: bool,
    pub dry_run: bool,
    /// Claude Code: a slot's targets, comma-separated `provider/model` ids (a bare id means
    /// the agent's own provider).
    pub opus: Option<String>,
    pub sonnet: Option<String>,
    pub haiku: Option<String>,
    pub fable: Option<String>,
    /// Codex: the bare id Codex starts with, its effort, and the model's targets in order.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub target: Vec<String>,
}

pub struct RestoreArgs {
    pub no_tui: bool,
    pub dry_run: bool,
    pub full: bool,
    pub force: bool,
    pub backup: Option<PathBuf>,
    pub remove_group: bool,
}

/// The ids of `provider` in the live catalog, bare (`claude-opus-5`), in catalog order.
pub fn provider_ids(catalog: &[CatalogModel], provider: &str) -> Vec<String> {
    catalog.iter().filter(|m| m.provider == provider).map(|m| m.upstream.clone()).collect()
}

/// Every id a target may name: the full `provider/model` ids of the live catalog, in order.
pub fn target_candidates(catalog: &[CatalogModel]) -> Vec<String> {
    catalog.iter().map(|m| m.id.clone()).collect()
}

/// Target ids from flags, resolved against the live catalog: a bare id means the agent's own
/// provider; an id the account cannot serve would only ever be skipped by the walk, which is
/// not what the user asked for. Order kept, duplicates dropped.
pub fn resolve_targets(catalog: &[CatalogModel], provider: &str, targets: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for id in targets {
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let full = if id.contains('/') { id.to_string() } else { format!("{provider}/{id}") };
        if !catalog.iter().any(|m| m.id == full) {
            bail!("{full}: not in your catalog (GET /cli/v1/models lists what your accounts can serve)");
        }
        if !out.contains(&full) {
            out.push(full);
        }
    }
    Ok(out)
}

/// The group's model set: `primary` names first, each with its targets as picked (the first
/// answers, the rest take over in order; a name picked twice keeps its first definition),
/// then every other id of the provider targeting itself, within the cap.
pub fn group_models(provider: &str, ids: &[String], primary: &[(String, Vec<String>)]) -> Vec<GroupModel> {
    let mut out: Vec<GroupModel> = Vec::new();
    for (name, picked) in primary {
        if out.iter().any(|m| &m.name == name) || picked.is_empty() {
            continue;
        }
        let mut targets: Vec<GroupTarget> = Vec::new();
        for t in picked {
            if !targets.iter().any(|x| &x.model == t) {
                targets.push(GroupTarget { model: t.clone(), account_id: None });
            }
        }
        out.push(GroupModel { name: name.clone(), targets });
    }
    for id in ids {
        if out.len() >= MAX_GROUP_MODELS {
            break;
        }
        if !out.iter().any(|m| &m.name == id) {
            out.push(GroupModel { name: id.clone(), targets: vec![GroupTarget { model: format!("{provider}/{id}"), account_id: None }] });
        }
    }
    out
}

/// A rerun keeps what the user did on the Groups page: targets they added to a computed name
/// stay (after the computed ones), and models under other names stay too, within the cap
/// (docs/cli.md § Keys and groups).
pub fn merge_with_existing(computed: Vec<GroupModel>, existing: &[GroupModel]) -> Vec<GroupModel> {
    let mut out = computed;
    for model in &mut out {
        if let Some(prev) = existing.iter().find(|e| e.name == model.name) {
            for target in &prev.targets {
                if !model.targets.iter().any(|t| t == target) {
                    model.targets.push(target.clone());
                }
            }
        }
    }
    for prev in existing {
        if out.len() >= MAX_GROUP_MODELS {
            break;
        }
        if !out.iter().any(|m| m.name == prev.name) {
            out.push(prev.clone());
        }
    }
    out
}

fn inject_key(value: &mut Value, key: &str) {
    match value {
        Value::String(s) if s == KEY_PLACEHOLDER => *s = key.to_string(),
        Value::Object(map) => map.values_mut().for_each(|v| inject_key(v, key)),
        Value::Array(items) => items.iter_mut().for_each(|v| inject_key(v, key)),
        _ => {}
    }
}

/// The recorded form of a written value: the plan's key placeholder becomes a marker, so
/// the record never holds the plaintext while every other value (a table's `base_url`, say)
/// stays comparable for drift.
fn record_value(value: &Value) -> Value {
    match value {
        Value::String(s) if s == KEY_PLACEHOLDER => Value::String(KEY_RECORD.to_string()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), record_value(v))).collect()),
        Value::Array(items) => Value::Array(items.iter().map(record_value).collect()),
        other => other.clone(),
    }
}

/// Whether a field's current value still matches what the CLI wrote. The key field cannot
/// be compared against a plaintext the state file never kept: it counts as intact while it
/// holds *a* proxy key where the CLI put one.
fn matches_written(field: &str, key_field: &str, current: Option<&Value>, wrote: &Value) -> bool {
    if field == key_field {
        return match (current, wrote) {
            (Some(Value::Object(obj)), Value::Object(expected)) => expected.iter().all(|(k, v)| match obj.get(k) {
                Some(cur) if v == KEY_RECORD => cur.as_str().is_some_and(|s| s.starts_with("sk-kano-proxy-")),
                Some(cur) => cur == v,
                None => false,
            }),
            (Some(Value::String(s)), _) => s.starts_with("sk-kano-proxy-"),
            _ => false,
        };
    }
    current == Some(wrote)
}

// ---------------------------------------------------------------------------
// setup

pub async fn cmd_setup(file: &StateFile, kind: AgentKind, args: SetupArgs) -> Result<()> {
    let state = file.load()?;
    api::require_signed_in(&state)?;
    let base_url = state.base_url.clone();
    let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
    let access = tokens.get(false).await?;

    let catalog = api::list_models(&base_url, &access).await?;
    // What is there now feeds the pickers: a rerun edits the current choices.
    let path = kind.config_path();
    let current_doc = Doc::load(kind.doc_kind(), &path)?;
    let groups = api::list_groups(&base_url, &access).await?;
    let existing: Option<&Group> = groups.iter().find(|g| g.slug == kind.group_slug());
    let base_field = match kind {
        AgentKind::ClaudeCode => "env.ANTHROPIC_BASE_URL",
        AgentKind::Codex => "model_providers.kano.base_url",
    };
    let configured = existing.is_some()
        && current_doc.get(base_field).and_then(|v| v.as_str().map(str::to_string)) == Some(kind.group_base(&base_url));
    let current = Current { doc: Some(&current_doc), group: existing, configured };
    let previous = state.agents.get(kind.id()).cloned();
    let device_name = state.device_name.as_deref().unwrap_or("device");

    // The root page (or the flags) — nothing below this line runs until the user saved.
    let plan = match kind {
        AgentKind::ClaudeCode => claude_code::plan_from(&base_url, &catalog, &claude_code::collect_picks(&args, &catalog, &current)?),
        AgentKind::Codex => codex::plan_from(&base_url, &catalog, &codex::collect_picks(&args, &catalog, &current)?),
    };
    // What the group will hold after the merge — what is printed and what is sent.
    let models = merge_with_existing(plan.group_models.clone(), existing.map(|g| g.models.as_slice()).unwrap_or(&[]));
    let lines = review_lines(kind, &plan, &models, existing.is_some(), &current_doc, previous.as_ref(), device_name, &path);
    if args.dry_run {
        println!("dry run — nothing is written, minted or changed\n");
        for line in lines {
            println!("{line}");
        }
        return Ok(());
    }
    // A record in the scrollback of exactly what is being written.
    for line in lines {
        println!("{line}");
    }
    println!();

    // 1. The group — looked up by slug so a rerun updates instead of duplicating.
    let group = match existing {
        Some(g) => api::update_group_models(&base_url, &access, &g.id, &models).await?,
        None => api::create_group(&base_url, &access, kind.group_name(), kind.group_slug(), &models).await?,
    };
    eprintln!("✓ group \"{}\" — {} models", group.slug, group.models.len());

    // 2. The key — the previous one this device minted is revoked first.
    if let Some(prev) = &previous {
        let _ = api::delete_key(&base_url, &access, &prev.key_id).await?;
    }
    let key_name = format!("{}@{}", kind.id(), state.device_name.as_deref().unwrap_or("device"));
    let key = api::create_key(&base_url, &access, &key_name).await?;
    eprintln!("✓ key \"{}\"", key.name);

    // 3. The config — backed up whole before the first write, every field recorded.
    let mut doc = current_doc;
    let backup_path = match &previous {
        Some(prev) => prev.backup_path.clone(),
        None => take_backup(file, kind, &path)?.map(|p| p.display().to_string()).unwrap_or_default(),
    };
    let mut fields: BTreeMap<String, FieldRecord> = BTreeMap::new();
    for (field, value) in &plan.fields {
        let before = match previous.as_ref().and_then(|p| p.fields.get(field)) {
            Some(record) => record.before.clone(),
            None => doc.get(field),
        };
        let mut written = value.clone();
        inject_key(&mut written, &key.key);
        doc.set(field, &written);
        fields.insert(field.clone(), FieldRecord { before, wrote: record_value(value) });
    }
    // A field the previous run wrote and this one does not (e.g. the catalog key when no
    // `codex` binary is around this time) goes back to its original value.
    if let Some(prev) = &previous {
        for (field, record) in &prev.fields {
            if !fields.contains_key(field) {
                match &record.before {
                    Some(v) => doc.set(field, v),
                    None => doc.remove(field),
                }
            }
        }
    }
    doc.write(&path)?;
    eprintln!("✓ wrote {}", path.display());

    // 4. Codex: the model catalog.
    let (catalog_path, catalog_backup_path) = match &plan.catalog {
        Some(cat) => {
            let backup = match previous.as_ref().and_then(|p| p.catalog_backup_path.clone()) {
                Some(b) => Some(b),
                None if previous.as_ref().is_some_and(|p| p.catalog_path.is_some()) => None,
                None => take_backup(file, kind, &cat.path)?.map(|p| p.display().to_string()),
            };
            codex::write_catalog(cat)?;
            eprintln!("✓ wrote {} ({} models)", cat.path.display(), cat.models.len());
            (Some(cat.path.display().to_string()), backup)
        }
        None => (None, None),
    };

    // 5. Record first, verify second: a failed verification must still be restorable.
    let record = AgentState {
        key_id: key.id.clone(),
        key_name: key.name.clone(),
        group_id: group.id.clone(),
        group_slug: group.slug.clone(),
        config_path: path.display().to_string(),
        backup_path,
        written_at: now_iso(),
        fields,
        catalog_path,
        catalog_backup_path,
    };
    file.mutate(|s| {
        s.agents.insert(kind.id().to_string(), record);
    })?;

    let group_base = kind.group_base(&base_url);
    let served = api::list_group_endpoint_models(&group_base, &key.key)
        .await
        .with_context(|| format!("the group endpoint did not answer — run `kano-proxy agent restore {}` to undo", kind.id()))?;
    let wanted: Vec<&str> = plan.group_models.iter().map(|m| m.name.as_str()).collect();
    let missing: Vec<&str> = wanted.iter().copied().filter(|n| !served.iter().any(|s| s == n)).collect();
    if !missing.is_empty() {
        bail!(
            "{group_base} does not list {} — run `kano-proxy agent restore {}` to undo",
            missing.join(", "),
            kind.id()
        );
    }
    eprintln!("✓ {group_base} lists {}", plan.summary_names());
    eprintln!("{} is set up — start it as usual.", kind.label());
    Ok(())
}

impl Plan {
    fn summary_names(&self) -> String {
        let names: Vec<&str> = self.group_models.iter().take(3).map(|m| m.name.as_str()).collect();
        let more = self.group_models.len().saturating_sub(3);
        if more > 0 {
            format!("{} and {more} more", names.join(", "))
        } else {
            names.join(", ")
        }
    }
}

/// The plan as lines — the review screen and `--dry-run` print the same thing. Values are
/// shown as they will be written, except the key, which does not exist yet.
#[allow(clippy::too_many_arguments)]
fn review_lines(
    kind: AgentKind,
    plan: &Plan,
    models: &[GroupModel],
    group_exists: bool,
    doc: &Doc,
    previous: Option<&AgentState>,
    device_name: &str,
    path: &Path,
) -> Vec<String> {
    let mut out: Vec<String> = plan.summary.clone();
    out.push(String::new());
    out.push(format!("Group \"{}\" — {}:", kind.group_slug(), if group_exists { "update" } else { "create" }));
    for model in models {
        let targets: Vec<&str> = model.targets.iter().map(|t| t.model.as_str()).collect();
        out.push(format!("  {:<32} → {}", model.name, targets.join(", ")));
    }
    out.push(String::new());
    out.push(format!(
        "Key \"{}@{device_name}\" — minted now{}",
        kind.id(),
        if previous.is_some() { ", the previous one is revoked first" } else { "" }
    ));
    out.push(String::new());
    out.push(format!(
        "{} — {}:",
        path.display(),
        if !path.exists() { "created" } else if previous.is_some() { "updated (the first backup is kept)" } else { "updated, backed up first" }
    ));
    for (field, value) in &plan.fields {
        let was = previous.and_then(|p| p.fields.get(field)).map(|r| r.before.clone()).unwrap_or_else(|| doc.get(field));
        out.push(format!(
            "  {field} = {}   (was: {})",
            render_value(value),
            was.map(|v| render_value(&v)).unwrap_or_else(|| "absent".into())
        ));
    }
    if let Some(catalog) = &plan.catalog {
        out.push(String::new());
        out.push(format!("{} — {} models from `codex debug models --bundled`", catalog.path.display(), catalog.models.len()));
    }
    out
}

/// Copy `path` into the backups directory before the first write ever made to it; `None`
/// when there is nothing to back up (the file did not exist).
fn take_backup(file: &StateFile, kind: AgentKind, path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let dir = backups_dir(&file.path, kind.id());
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("config");
    let target = dir.join(format!("{}-{name}", now_stamp()));
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    doc::write_private(&target, &bytes)?;
    eprintln!("✓ backed up {} → {}", path.display(), target.display());
    Ok(Some(target))
}

fn render_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// restore

pub async fn cmd_restore(file: &StateFile, kind: AgentKind, args: RestoreArgs) -> Result<()> {
    let state = file.load()?;
    let Some(record) = state.agents.get(kind.id()).cloned() else {
        bail!("{} is not set up on this device (nothing recorded in {})", kind.label(), file.path.display());
    };
    let path = PathBuf::from(&record.config_path);
    let doc = Doc::load(kind.doc_kind(), &path)?;

    // Drift: fields the user edited since the CLI wrote them.
    let drifted: Vec<String> = record
        .fields
        .iter()
        .filter(|(field, r)| !matches_written(field, kind.key_field(), doc.get(field).as_ref(), &r.wrote))
        .map(|(field, _)| field.clone())
        .collect();

    let backup = if args.full { Some(resolve_backup(&record, args.backup.as_deref())?) } else { None };

    let mut lines: Vec<String> = vec![format!("Restore {} ({}):", kind.label(), if args.full { "full file" } else { "fields" })];
    if let Some(backup) = &backup {
        match backup {
            Some(b) => {
                lines.push(format!("  {} ← {}", path.display(), b.display()));
                let current = fs::read_to_string(&path).unwrap_or_default();
                let original = fs::read_to_string(b).with_context(|| format!("cannot read {}", b.display()))?;
                lines.extend(line_diff(&current, &original).iter().take(80).map(|l| format!("    {l}")));
            }
            None => lines.push(format!("  {} did not exist before setup — it will be deleted", path.display())),
        }
    } else {
        for (field, r) in &record.fields {
            let back = r.before.as_ref().map(render_value).unwrap_or_else(|| "absent".into());
            lines.push(format!("  {field} ← {back}{}", if drifted.contains(field) { "   (edited since setup!)" } else { "" }));
        }
    }
    if let Some(cat) = &record.catalog_path {
        match &record.catalog_backup_path {
            Some(b) => lines.push(format!("  {cat} ← {b}")),
            None => lines.push(format!("  {cat} will be deleted")),
        }
    }
    lines.push(format!(
        "  key \"{}\" will be revoked; group \"{}\" {}",
        record.key_name,
        record.group_slug,
        if args.remove_group { "will be deleted" } else { "stays (pass --remove-group to delete it)" }
    ));
    if args.full {
        lines.push(String::new());
        lines.push(format!("The whole file {} is replaced by the backup. Every change made since is lost.", path.display()));
    }
    if !drifted.is_empty() {
        lines.push(String::new());
        lines.push("These fields were edited since the CLI wrote them — restoring overwrites the edits:".into());
        lines.extend(drifted.iter().map(|f| format!("  {f}")));
    }
    if args.dry_run || args.no_tui {
        for line in &lines {
            println!("{line}");
        }
    }
    if args.dry_run {
        println!("\ndry run — nothing changed");
        return Ok(());
    }

    // Interactive: always show the plan and wait for Enter. Non-interactive: only a full
    // replace or overwriting the user's own edits needs `--force`.
    if args.no_tui {
        if (args.full || !drifted.is_empty()) && !args.force {
            bail!(if args.full {
                "--full replaces the whole file — pass --force to confirm with --no-tui"
            } else {
                "fields were edited since setup — pass --force to overwrite them with --no-tui"
            });
        }
    } else if !args.force {
        tui::require_tty()?;
        if tui::review(&format!("kano-proxy agent restore {}", kind.id()), &lines, "restores all of this")? != Nav::Next(()) {
            return Err(tui::cancelled());
        }
    }

    // The file.
    if let Some(backup) = backup {
        if path.exists() {
            let pre = backups_dir(&file.path, kind.id()).join(format!(
                "{}-{}.pre-restore",
                now_stamp(),
                path.file_name().and_then(|n| n.to_str()).unwrap_or("config")
            ));
            doc::write_private(&pre, &fs::read(&path)?)?;
            eprintln!("✓ current file kept at {}", pre.display());
        }
        match backup {
            Some(b) => {
                doc::write_private(&path, &fs::read(&b)?)?;
                eprintln!("✓ restored {} from {}", path.display(), b.display());
            }
            None => {
                let _ = fs::remove_file(&path);
                eprintln!("✓ removed {}", path.display());
            }
        }
    } else {
        let mut doc = doc;
        for (field, r) in &record.fields {
            match &r.before {
                Some(v) => doc.set(field, v),
                None => doc.remove(field),
            }
        }
        doc.write(&path)?;
        eprintln!("✓ restored {} fields in {}", record.fields.len(), path.display());
    }
    if let Some(cat) = &record.catalog_path {
        match &record.catalog_backup_path {
            Some(b) => {
                doc::write_private(Path::new(cat), &fs::read(b)?)?;
                eprintln!("✓ restored {cat}");
            }
            None => {
                let _ = fs::remove_file(cat);
                eprintln!("✓ removed {cat}");
            }
        }
    }

    // The server side — tolerant of "already gone".
    if state.signed_in() {
        let tokens = TokenCache::new(Arc::new(StateFile::new(file.path.clone())));
        match tokens.get(false).await {
            Ok(access) => {
                match api::delete_key(&state.base_url, &access, &record.key_id).await? {
                    true => eprintln!("✓ revoked key \"{}\"", record.key_name),
                    false => eprintln!("· key \"{}\" was already revoked", record.key_name),
                }
                if args.remove_group {
                    // The review above already named the group as deleted; this second
                    // question exists because other devices may depend on it.
                    let confirmed = args.force || args.no_tui || {
                        tui::require_tty()?;
                        tui::confirm(
                            &format!("kano-proxy agent restore {} --remove-group", kind.id()),
                            &[&format!("Delete the model group \"{}\"? Other devices or clients on its endpoint stop resolving.", record.group_slug)],
                            "Yes — delete the group",
                        )?
                    };
                    if confirmed {
                        match api::delete_group(&state.base_url, &access, &record.group_id).await? {
                            true => eprintln!("✓ deleted group \"{}\"", record.group_slug),
                            false => eprintln!("· group \"{}\" was already deleted", record.group_slug),
                        }
                    }
                }
            }
            Err(e) => eprintln!("· server side skipped ({e:#}) — revoke key \"{}\" on the Access page", record.key_name),
        }
    } else {
        eprintln!("· not signed in — revoke key \"{}\" on the Access page", record.key_name);
    }

    file.mutate(|s| {
        s.agents.remove(kind.id());
    })?;
    eprintln!("{} restored.", kind.label());
    Ok(())
}

/// Which backup a full restore uses: `--backup`, else the one recorded at setup (the file
/// from before the first write), else `None` when the file did not exist back then.
fn resolve_backup(record: &AgentState, flag: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(p) = flag {
        if !p.exists() {
            bail!("--backup {}: no such file", p.display());
        }
        return Ok(Some(p.to_path_buf()));
    }
    if record.backup_path.is_empty() {
        return Ok(None);
    }
    let p = PathBuf::from(&record.backup_path);
    if !p.exists() {
        bail!("the backup {} is gone — pass --backup <path> or restore field by field", p.display());
    }
    Ok(Some(p))
}

// ---------------------------------------------------------------------------
// list / status

pub struct AgentRow {
    pub kind: AgentKind,
    pub record: Option<AgentState>,
    /// `ok`, `drifted`, `missing` (the config file is gone), or `-` when not managed.
    pub drift: &'static str,
}

pub fn rows(state: &crate::state::State) -> Vec<AgentRow> {
    AgentKind::ALL
        .iter()
        .map(|&kind| {
            let record = state.agents.get(kind.id()).cloned();
            let drift = match &record {
                None => "-",
                Some(r) => {
                    let path = PathBuf::from(&r.config_path);
                    if !path.exists() {
                        "missing"
                    } else {
                        match Doc::load(kind.doc_kind(), &path) {
                            Ok(doc) if r.fields.iter().all(|(f, rec)| matches_written(f, kind.key_field(), doc.get(f).as_ref(), &rec.wrote)) => "ok",
                            _ => "drifted",
                        }
                    }
                }
            };
            AgentRow { kind, record, drift }
        })
        .collect()
}

pub fn print_rows(state: &crate::state::State) {
    println!("{:<12} {:<9} {:<12} {:<24} {:<22} DRIFT", "AGENT", "MANAGED", "GROUP", "KEY", "WRITTEN");
    for row in rows(state) {
        match row.record {
            Some(r) => println!("{:<12} {:<9} {:<12} {:<24} {:<22} {}", row.kind.id(), "yes", r.group_slug, r.key_name, r.written_at, row.drift),
            None => println!("{:<12} {:<9} {:<12} {:<24} {:<22} {}", row.kind.id(), "no", "-", "-", "-", row.drift),
        }
    }
}

pub fn cmd_list(file: &StateFile) -> Result<()> {
    let state = file.load()?;
    print_rows(&state);
    Ok(())
}

// ---------------------------------------------------------------------------
// time

/// `2026-10-11T08:30:00Z` from the system clock, no dependency.
pub fn now_iso() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let (y, m, d, hh, mm, ss) = civil(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// `20261011-083000` — a filename-safe stamp for backups.
pub fn now_stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let (y, m, d, hh, mm, ss) = civil(secs);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

/// Unix seconds → civil UTC (Howard Hinnant's days-from-civil, inverted).
fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    ((y), m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32)
}

/// A plain line diff (LCS) for the `--full` preview: `-` lines leave, `+` lines come back.
pub fn line_diff(current: &str, original: &str) -> Vec<String> {
    let a: Vec<&str> = current.lines().collect();
    let b: Vec<&str> = original.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if a[i] == b[j] {
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(format!("- {}", a[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| format!("- {l}")));
    out.extend(b[j..].iter().map(|l| format!("+ {l}")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- end to end against a scripted server ---------------------------------

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc as StdArc, Mutex};

    /// A one-thread HTTP/1.1 responder: records every request (method, path, body) and
    /// answers by path, which is all `agent setup` / `restore` need from the server.
    struct MockServer {
        base: String,
        requests: StdArc<Mutex<Vec<(String, String, String)>>>,
    }

    fn mock_server(groups_initially_present: bool) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = StdArc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        let group_exists = StdArc::new(Mutex::new(groups_initially_present));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.is_empty() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h == "\r\n" || h.is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                if len > 0 {
                    reader.read_exact(&mut body).unwrap();
                }
                let body = String::from_utf8_lossy(&body).to_string();
                log.lock().unwrap().push((method.clone(), path.clone(), body.clone()));
                let (status, json) = match (method.as_str(), path.as_str()) {
                    ("POST", "/cli/v1/token") => (200, json!({ "refresh_token": "kpr_next", "access_token": "acc.tok", "expires_in": 3600 }).to_string()),
                    ("GET", "/cli/v1/models") => (200, json!({ "object": "list", "data": [
                        { "id": "claude-code/claude-opus-5", "provider": "claude-code", "upstream": "claude-opus-5", "display_name": "Opus", "available": true },
                        { "id": "claude-code/claude-sonnet-5", "provider": "claude-code", "upstream": "claude-sonnet-5", "display_name": "Sonnet", "available": true },
                        { "id": "claude-code/claude-haiku-4-5", "provider": "claude-code", "upstream": "claude-haiku-4-5", "display_name": "Haiku", "available": true },
                        { "id": "claude-code/claude-fable-5-1", "provider": "claude-code", "upstream": "claude-fable-5-1", "display_name": "Fable", "available": true },
                        { "id": "codex/gpt-5.6-sol", "provider": "codex", "upstream": "gpt-5.6-sol", "display_name": "GPT", "available": true }
                    ], "providers": [] }).to_string()),
                    ("GET", "/cli/v1/model-groups") => {
                        let exists = *group_exists.lock().unwrap();
                        let groups = if exists {
                            json!([{ "id": "mgrp_1", "name": "Claude Code", "slug": "claude-code", "models": [
                                { "name": "claude-opus-5", "targets": [{ "model": "grok/grok-4.5", "account_id": null }] },
                                { "name": "my-alias", "targets": [{ "model": "codex/gpt-5.6-sol", "account_id": null }] }
                            ]}])
                        } else {
                            json!([])
                        };
                        (200, json!({ "groups": groups }).to_string())
                    }
                    ("POST", "/cli/v1/model-groups") => {
                        let sent: Value = serde_json::from_str(&body).unwrap();
                        if sent["slug"] == "claude-code" {
                            *group_exists.lock().unwrap() = true;
                        }
                        (201, json!({ "id": if sent["slug"] == "codex-cli" { "mgrp_2" } else { "mgrp_1" }, "name": sent["name"], "slug": sent["slug"], "models": sent["models"] }).to_string())
                    }
                    ("PUT", "/cli/v1/model-groups/mgrp_1") => {
                        let sent: Value = serde_json::from_str(&body).unwrap();
                        (200, json!({ "id": "mgrp_1", "name": "Claude Code", "slug": "claude-code", "models": sent["models"] }).to_string())
                    }
                    ("POST", "/cli/v1/keys") => {
                        let sent: Value = serde_json::from_str(&body).unwrap();
                        (201, json!({ "id": format!("key_{}", log.lock().unwrap().len()), "name": sent["name"], "key": "sk-kano-proxy-e2e-secret", "device_id": "clidev_1" }).to_string())
                    }
                    ("DELETE", p) if p.starts_with("/cli/v1/keys/key_") => (200, json!({ "ok": true }).to_string()),
                    ("DELETE", "/cli/v1/keys/key_gone") => (404, json!({ "error": "not found" }).to_string()),
                    ("GET", "/g/claude-code/anthropic/v1/models") => (200, json!({ "data": [
                        { "id": "claude-opus-5" }, { "id": "claude-sonnet-5" }, { "id": "claude-haiku-4-5" }, { "id": "claude-fable-5-1" }, { "id": "my-alias" }
                    ]}).to_string()),
                    ("GET", "/g/codex-cli/openai/v1/models") => (200, json!({ "object": "list", "data": [{ "id": "gpt-5.6-sol" }] }).to_string()),
                    ("DELETE", "/cli/v1/device") => (200, json!({ "ok": true }).to_string()),
                    _ => (404, json!({ "error": format!("unscripted {method} {path}") }).to_string()),
                };
                let reason = match status { 200 => "OK", 201 => "Created", _ => "Not Found" };
                let _ = write!(stream, "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len());
                let _ = stream.flush();
            }
        });
        MockServer { base, requests }
    }

    struct Sandbox {
        dir: PathBuf,
        file: StateFile,
    }

    impl Sandbox {
        fn new(base: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("kano-proxy-agent-e2e-{}-{}-{n}", std::process::id(), now_stamp()));
            fs::create_dir_all(&dir).unwrap();
            let file = StateFile::new(dir.join("state.json"));
            file.save(&crate::state::State {
                base_url: base.to_string(),
                device_id: Some("clidev_1".into()),
                device_name: Some("my-mac".into()),
                refresh_token: Some("kpr_first".into()),
                ..Default::default()
            })
            .unwrap();
            Self { dir, file }
        }
        fn settings(&self) -> PathBuf {
            self.dir.join(".claude").join("settings.json")
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn claude_args(dry_run: bool) -> SetupArgs {
        SetupArgs {
            no_tui: true,
            dry_run,
            // Opus answers on codex first and is still served under the Claude id.
            opus: Some("codex/gpt-5.6-sol,claude-opus-5".into()),
            sonnet: Some("claude-sonnet-5".into()),
            haiku: Some("claude-haiku-4-5".into()),
            fable: None,
            model: None,
            effort: None,
            target: vec![],
        }
    }

    /// The whole `agent setup` → `list` → rerun → `restore` loop for Claude Code against a
    /// scripted server, with HOME pointed at a sandbox: the group is created then updated
    /// (the user's extra target and extra model kept), the key is minted and never stored,
    /// the file is backed up once and restored field by field, the previous key is revoked
    /// on rerun, and the record disappears on restore.
    #[tokio::test(flavor = "multi_thread")]
    async fn setup_and_restore_claude_code_end_to_end() {
        let server = mock_server(false);
        let sandbox = Sandbox::new(&server.base);
        let _guard = HomeGuard::set(&sandbox.dir);
        let settings = sandbox.settings();
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(&settings, "{\n  \"permissions\": {\"allow\": [\"Bash\"]},\n  \"env\": {\"ANTHROPIC_MODEL\": \"old-model\", \"OTHER\": \"keep\"}\n}\n").unwrap();

        // A dry run reads but writes, mints and creates nothing.
        cmd_setup(&sandbox.file, AgentKind::ClaudeCode, claude_args(true)).await.unwrap();
        {
            let seen = server.requests.lock().unwrap();
            assert!(seen.iter().all(|(m, p, _)| m == "GET" || p == "/cli/v1/token"), "{seen:?}");
            assert!(sandbox.file.load().unwrap().agents.is_empty());
        }

        cmd_setup(&sandbox.file, AgentKind::ClaudeCode, claude_args(false)).await.unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(written["permissions"]["allow"][0], "Bash", "untouched keys survive");
        assert_eq!(written["env"]["OTHER"], "keep");
        assert_eq!(written["env"]["ANTHROPIC_BASE_URL"], format!("{}/g/claude-code/anthropic", server.base));
        assert_eq!(written["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-kano-proxy-e2e-secret");
        assert_eq!(written["env"]["ANTHROPIC_MODEL"], "old-model", "the user's own start model is not touched");
        assert_eq!(written["env"]["CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"], "1");
        assert_eq!(written["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "claude-opus-5");
        assert_eq!(written["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"], "claude-sonnet-5");
        assert!(written["env"].get("ANTHROPIC_DEFAULT_FABLE_MODEL").is_none(), "a slot without targets is not written");
        let state = sandbox.file.load().unwrap();
        let record = state.agents.get("claude-code").expect("recorded");
        assert_eq!(record.key_name, "claude-code@my-mac");
        assert_eq!(record.group_slug, "claude-code");
        assert!(!fs::read_to_string(&sandbox.file.path).unwrap().contains("sk-kano-proxy-e2e-secret"), "no plaintext key in the state file");
        assert!(!record.fields.contains_key("env.ANTHROPIC_MODEL"));
        assert_eq!(record.fields["env.ANTHROPIC_BASE_URL"].before, None);
        assert_eq!(record.fields["env.ANTHROPIC_DEFAULT_OPUS_MODEL"].wrote, json!("claude-opus-5"));
        assert_eq!(record.fields["env.ANTHROPIC_AUTH_TOKEN"].wrote, json!(KEY_RECORD));
        assert!(Path::new(&record.backup_path).exists(), "the pre-setup backup exists");
        assert!(fs::read_to_string(&record.backup_path).unwrap().contains("old-model"));
        {
            let seen = server.requests.lock().unwrap();
            let created = seen.iter().find(|(m, p, _)| m == "POST" && p == "/cli/v1/model-groups").expect("group created");
            let body: Value = serde_json::from_str(&created.2).unwrap();
            assert_eq!(body["slug"], "claude-code");
            assert_eq!(body["models"][0]["name"], "claude-opus-5");
            assert_eq!(body["models"][0]["targets"][0]["model"], "codex/gpt-5.6-sol", "targets in the order picked");
            assert_eq!(body["models"][0]["targets"][1]["model"], "claude-code/claude-opus-5");
            assert!(body["models"].as_array().unwrap().iter().any(|m| m["name"] == "claude-fable-5-1"), "every claude-code id is mirrored");
            assert!(seen.iter().any(|(m, p, _)| m == "GET" && p == "/g/claude-code/anthropic/v1/models"), "verified through the group endpoint");
        }
        assert_eq!(rows(&state)[0].drift, "ok");

        // The user edits a managed field; list reports drift.
        let mut edited: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        edited["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"] = json!("claude-opus-5");
        fs::write(&settings, serde_json::to_string_pretty(&edited).unwrap()).unwrap();
        assert_eq!(rows(&sandbox.file.load().unwrap())[0].drift, "drifted");

        // A rerun: the group is updated (user target + extra model kept), the old key revoked,
        // the original "before" values and backup kept.
        let first_backup = record.backup_path.clone();
        let first_key = record.key_id.clone();
        cmd_setup(&sandbox.file, AgentKind::ClaudeCode, claude_args(false)).await.unwrap();
        let state = sandbox.file.load().unwrap();
        let record = &state.agents["claude-code"];
        assert_eq!(record.backup_path, first_backup);
        assert_ne!(record.key_id, first_key);
        assert_eq!(record.fields["env.ANTHROPIC_DEFAULT_SONNET_MODEL"].before, None, "the first run's \"before\" is kept across reruns");
        {
            let seen = server.requests.lock().unwrap();
            assert!(seen.iter().any(|(m, p, _)| m == "DELETE" && p == &format!("/cli/v1/keys/{first_key}")), "the previous key is revoked");
            let updated = seen.iter().rev().find(|(m, p, _)| m == "PUT" && p == "/cli/v1/model-groups/mgrp_1").expect("group updated, not duplicated");
            let body: Value = serde_json::from_str(&updated.2).unwrap();
            let opus = &body["models"][0];
            let targets: Vec<&str> = opus["targets"].as_array().unwrap().iter().map(|t| t["model"].as_str().unwrap()).collect();
            assert_eq!(targets, ["codex/gpt-5.6-sol", "claude-code/claude-opus-5", "grok/grok-4.5"], "the user's own target stays after the picked ones");
            assert!(body["models"].as_array().unwrap().iter().any(|m| m["name"] == "my-alias"), "the user's extra model stays");
        }

        // Restore, field by field: edited values are only overwritten with --force under --no-tui.
        let restore = |force: bool, full: bool| RestoreArgs { no_tui: true, dry_run: false, full, force, backup: None, remove_group: false };
        cmd_restore(&sandbox.file, AgentKind::ClaudeCode, restore(false, false)).await.unwrap();
        let restored: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored["env"]["ANTHROPIC_MODEL"], "old-model");
        assert_eq!(restored["env"]["OTHER"], "keep");
        assert!(restored["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert!(restored["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert_eq!(restored["permissions"]["allow"][0], "Bash");
        assert!(sandbox.file.load().unwrap().agents.is_empty(), "the record is gone");
        assert!(Path::new(&first_backup).exists(), "backups are never deleted");
        {
            let seen = server.requests.lock().unwrap();
            assert!(seen.iter().any(|(m, p, _)| m == "DELETE" && p == &format!("/cli/v1/keys/{}", record.key_id)));
            assert!(!seen.iter().any(|(m, p, _)| m == "DELETE" && p.starts_with("/cli/v1/model-groups")), "the group stays without --remove-group");
        }
        assert!(cmd_restore(&sandbox.file, AgentKind::ClaudeCode, restore(false, false)).await.is_err(), "nothing to restore twice");

        // Full restore after a fresh setup with drift: refused without --force, then the backup returns whole.
        cmd_setup(&sandbox.file, AgentKind::ClaudeCode, claude_args(false)).await.unwrap();
        fs::write(&settings, "{\"env\": {\"ANTHROPIC_AUTH_TOKEN\": \"sk-kano-proxy-x\"}, \"hand\": true}").unwrap();
        assert!(cmd_restore(&sandbox.file, AgentKind::ClaudeCode, restore(false, true)).await.is_err());
        cmd_restore(&sandbox.file, AgentKind::ClaudeCode, restore(true, true)).await.unwrap();
        let back = fs::read_to_string(&settings).unwrap();
        assert!(back.contains("old-model") && back.contains("\"OTHER\": \"keep\"") && !back.contains("hand"), "{back}");
        let pre: Vec<_> = fs::read_dir(backups_dir(&sandbox.file.path, "claude-code")).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".pre-restore")).collect();
        assert_eq!(pre.len(), 1, "the overwritten file was kept");
    }

    /// The Codex path: the TOML is edited in place (comments and other keys kept), the
    /// `[model_providers.kano]` table carries the key, the catalog step depends on a `codex`
    /// binary (absent here, so `model_catalog_json` is not written), and restore removes
    /// exactly what setup added.
    #[tokio::test(flavor = "multi_thread")]
    async fn setup_and_restore_codex_end_to_end() {
        let server = mock_server(false);
        let sandbox = Sandbox::new(&server.base);
        let _guard = HomeGuard::set(&sandbox.dir);
        let _path = PathGuard::empty();
        let config = sandbox.dir.join(".codex").join("config.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "# mine\nmodel = \"o3\" # was\nsandbox_mode = \"workspace-write\"\n\n[model_providers.other]\nname = \"Other\"\n").unwrap();

        let args = SetupArgs {
            no_tui: true,
            dry_run: false,
            opus: None,
            sonnet: None,
            haiku: None,
            fable: None,
            model: Some("gpt-5.6-sol".into()),
            effort: Some("xhigh".into()),
            target: vec!["gpt-5.6-sol".into(), "claude-code/claude-opus-5".into()],
        };
        cmd_setup(&sandbox.file, AgentKind::Codex, args).await.unwrap();
        let written = fs::read_to_string(&config).unwrap();
        assert!(written.contains("# mine") && written.contains("# was"), "{written}");
        assert!(written.contains("sandbox_mode = \"workspace-write\""));
        assert!(written.contains("[model_providers.other]"));
        let doc = Doc::parse(Kind::Toml, &written).unwrap();
        assert_eq!(doc.get("model"), Some(json!("gpt-5.6-sol")));
        assert_eq!(doc.get("model_provider"), Some(json!("kano")));
        assert_eq!(doc.get("model_reasoning_effort"), Some(json!("xhigh")));
        assert_eq!(doc.get("model_catalog_json"), None, "no codex binary, no catalog key");
        assert_eq!(doc.get("model_providers.kano.base_url"), Some(json!(format!("{}/g/codex-cli/openai/v1", server.base))));
        assert_eq!(doc.get("model_providers.kano.experimental_bearer_token"), Some(json!("sk-kano-proxy-e2e-secret")));
        assert_eq!(doc.get("model_providers.kano.wire_api"), Some(json!("responses")));
        let state = sandbox.file.load().unwrap();
        let record = &state.agents["codex"];
        assert_eq!(record.fields["model"].before, Some(json!("o3")));
        assert_eq!(record.fields["model_providers.kano"].wrote["experimental_bearer_token"], json!(KEY_RECORD));
        assert!(record.catalog_path.is_none());
        assert_eq!(rows(&state)[1].drift, "ok");
        {
            let seen = server.requests.lock().unwrap();
            let created = seen.iter().find(|(m, p, _)| m == "POST" && p == "/cli/v1/model-groups").unwrap();
            let body: Value = serde_json::from_str(&created.2).unwrap();
            assert_eq!(body["slug"], "codex-cli");
            assert_eq!(body["models"][0]["name"], "gpt-5.6-sol");
            assert_eq!(body["models"][0]["targets"][1]["model"], "claude-code/claude-opus-5");
            assert!(seen.iter().any(|(m, p, _)| m == "GET" && p == "/g/codex-cli/openai/v1/models"));
        }

        cmd_restore(&sandbox.file, AgentKind::Codex, RestoreArgs { no_tui: true, dry_run: false, full: false, force: false, backup: None, remove_group: false }).await.unwrap();
        let restored = fs::read_to_string(&config).unwrap();
        let doc = Doc::parse(Kind::Toml, &restored).unwrap();
        assert_eq!(doc.get("model"), Some(json!("o3")));
        assert_eq!(doc.get("model_provider"), None);
        assert_eq!(doc.get("model_reasoning_effort"), None);
        assert_eq!(doc.get("model_providers.kano"), None);
        assert_eq!(doc.get("model_providers.other.name"), Some(json!("Other")));
        assert!(restored.contains("# mine") && restored.contains("# was"), "{restored}");
        assert!(sandbox.file.load().unwrap().agents.is_empty());
    }

    /// `PATH` emptied so no `codex` binary is found, whatever the machine has.
    struct PathGuard(Option<std::ffi::OsString>);

    impl PathGuard {
        fn empty() -> Self {
            let previous = std::env::var_os("PATH");
            std::env::set_var("PATH", "");
            Self(previous)
        }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    /// `HOME` is process-global: tests that need it run one at a time.
    struct HomeGuard {
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    static HOME_LOCK: Mutex<()> = Mutex::new(());

    impl HomeGuard {
        fn set(dir: &Path) -> Self {
            let lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            #[cfg(windows)]
            let var = "USERPROFILE";
            #[cfg(not(windows))]
            let var = "HOME";
            let previous = std::env::var_os(var);
            std::env::set_var(var, dir);
            Self { previous, _lock: lock }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            #[cfg(windows)]
            let var = "USERPROFILE";
            #[cfg(not(windows))]
            let var = "HOME";
            match &self.previous {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
    }

    fn cat(ids: &[&str]) -> Vec<CatalogModel> {
        ids.iter()
            .map(|id| {
                let (p, u) = id.split_once('/').unwrap();
                CatalogModel { id: id.to_string(), provider: p.into(), upstream: u.into() }
            })
            .collect()
    }

    #[test]
    fn group_models_put_primaries_first_and_respect_the_cap() {
        let ids: Vec<String> = (0..30).map(|i| format!("claude-{i}")).collect();
        let primary = vec![
            ("claude-25".to_string(), vec!["claude-code/claude-25".to_string(), "codex/gpt".to_string()]),
            ("claude-3".to_string(), vec!["claude-code/claude-3".to_string()]),
            ("claude-9".to_string(), vec![]),
        ];
        let models = group_models("claude-code", &ids, &primary);
        assert_eq!(models.len(), MAX_GROUP_MODELS);
        assert_eq!(models[0].name, "claude-25");
        assert_eq!(models[0].targets.iter().map(|t| t.model.as_str()).collect::<Vec<_>>(), ["claude-code/claude-25", "codex/gpt"]);
        assert_eq!(models[1].name, "claude-3");
        assert_eq!(models[2].name, "claude-0");
        assert_eq!(models.iter().filter(|m| m.name == "claude-3").count(), 1, "no duplicate names");
        let nine = models.iter().find(|m| m.name == "claude-9").unwrap();
        assert_eq!(nine.targets[0].model, "claude-code/claude-9", "a name with no picks is mirrored like any other id");
    }

    #[test]
    fn merge_keeps_user_targets_and_extra_models() {
        let computed = group_models("claude-code", &["claude-opus-5".to_string()], &[("claude-opus-5".to_string(), vec!["claude-code/claude-opus-5".to_string()])]);
        let existing = vec![
            GroupModel {
                name: "claude-opus-5".into(),
                targets: vec![
                    GroupTarget { model: "claude-code/claude-opus-5".into(), account_id: Some("acc_1".into()) },
                    GroupTarget { model: "grok/grok-4.5".into(), account_id: None },
                ],
            },
            GroupModel { name: "my-alias".into(), targets: vec![GroupTarget { model: "codex/gpt".into(), account_id: None }] },
        ];
        let merged = merge_with_existing(computed, &existing);
        assert_eq!(merged.len(), 2);
        let t: Vec<String> = merged[0].targets.iter().map(|t| format!("{}:{}", t.model, t.account_id.as_deref().unwrap_or("-"))).collect();
        assert_eq!(t, ["claude-code/claude-opus-5:-", "claude-code/claude-opus-5:acc_1", "grok/grok-4.5:-"]);
        assert_eq!(merged[1].name, "my-alias");
    }

    #[test]
    fn targets_must_exist_in_the_catalog() {
        let catalog = cat(&["claude-code/claude-opus-5", "codex/gpt-5.6-sol"]);
        assert_eq!(
            resolve_targets(&catalog, "claude-code", &["codex/gpt-5.6-sol".into(), " codex/gpt-5.6-sol ".into(), "claude-opus-5".into()]).unwrap(),
            ["codex/gpt-5.6-sol", "claude-code/claude-opus-5"],
            "order kept, duplicates dropped, a bare id is the agent's own provider"
        );
        assert!(resolve_targets(&catalog, "claude-code", &["grok/grok-4.5".into()]).is_err());
        assert!(resolve_targets(&catalog, "claude-code", &["gpt-5.6-sol".into()]).is_err(), "bare means claude-code/, not any provider");
        assert_eq!(provider_ids(&catalog, "claude-code"), ["claude-opus-5"]);
        assert_eq!(target_candidates(&catalog), ["claude-code/claude-opus-5", "codex/gpt-5.6-sol"]);
    }

    #[test]
    fn the_key_is_injected_but_never_recorded() {
        let mut v = json!({ "name": "Kano Proxy", "experimental_bearer_token": KEY_PLACEHOLDER, "nested": [KEY_PLACEHOLDER] });
        inject_key(&mut v, "sk-kano-proxy-abc");
        assert_eq!(v["experimental_bearer_token"], "sk-kano-proxy-abc");
        assert_eq!(v["nested"][0], "sk-kano-proxy-abc");
        assert_eq!(record_value(&json!(KEY_PLACEHOLDER)), json!(KEY_RECORD));
        assert_eq!(record_value(&json!("v")), json!("v"));
        assert_eq!(record_value(&json!({ "name": "Kano Proxy", "experimental_bearer_token": KEY_PLACEHOLDER })), json!({ "name": "Kano Proxy", "experimental_bearer_token": KEY_RECORD }));
        // Drift: the key field is intact while it holds a proxy key; other fields compare exactly.
        assert!(matches_written("k", "k", Some(&json!("sk-kano-proxy-zzz")), &json!(KEY_RECORD)));
        assert!(!matches_written("k", "k", Some(&json!("sk-other")), &json!(KEY_RECORD)));
        assert!(!matches_written("k", "k", None, &json!(KEY_RECORD)));
        let table = json!({ "name": "Kano Proxy", "experimental_bearer_token": KEY_RECORD });
        assert!(matches_written("model_providers.kano", "model_providers.kano", Some(&json!({ "name": "Kano Proxy", "experimental_bearer_token": "sk-kano-proxy-1", "extra": true })), &table));
        assert!(!matches_written("model_providers.kano", "model_providers.kano", Some(&json!({ "name": "Other", "experimental_bearer_token": "sk-kano-proxy-1" })), &table));
        assert!(matches_written("model", "k", Some(&json!("a")), &json!("a")));
        assert!(!matches_written("model", "k", Some(&json!("b")), &json!("a")));
    }

    #[test]
    fn civil_time_and_diff() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(1_760_000_000), (2025, 10, 9, 8, 53, 20));
        assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0));
        let diff = line_diff("a\nb\nc\n", "a\nx\nc\n");
        assert_eq!(diff, ["- b", "+ x"]);
        assert!(line_diff("same\n", "same\n").is_empty());
    }

    #[test]
    fn group_bases_follow_the_agents_wire() {
        assert_eq!(AgentKind::ClaudeCode.group_base("https://p.example"), "https://p.example/g/claude-code/anthropic");
        assert_eq!(AgentKind::Codex.group_base("https://p.example"), "https://p.example/g/codex-cli/openai/v1");
        assert!(AgentKind::ClaudeCode.config_path().ends_with(Path::new(".claude").join("settings.json")));
        assert!(AgentKind::Codex.config_path().ends_with(Path::new(".codex").join("config.toml")));
    }
}
