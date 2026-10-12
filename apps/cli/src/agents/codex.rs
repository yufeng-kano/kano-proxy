//! Codex CLI (docs/cli.md § Command surface, the docs site's Codex CLI page):
//! a `[model_providers.kano]` table and the top-level model keys in
//! `config.toml`, pointed at the `codex-cli` group's OpenAI endpoint, plus the
//! model catalog generated from the installed `codex` binary so the `/model`
//! picker shows what this account can serve, with efforts the proxy accepts.
//! The model Codex starts with is a name from its own catalog; the targets
//! behind that name are picked from the whole proxy catalog, in order.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::{doc, group_models, provider_ids, resolve_targets, target_candidates, AgentKind, Current, Plan, SetupArgs, EFFORTS, KEY_PLACEHOLDER};
use crate::api::CatalogModel;
use crate::tui::{self, Nav};

pub const KEY_FIELD: &str = "model_providers.kano";
pub const CATALOG_FILE: &str = "kano-models.json";
const DEFAULT_EFFORT: &str = "high";

#[derive(Debug, Clone)]
pub struct Picks {
    pub model: String,
    pub effort: String,
    /// The `provider/model` targets behind `model`, first answers; never empty.
    pub targets: Vec<String>,
}

/// The catalog file `setup` writes: `~/.codex/kano-models.json`, derived from the bundled
/// catalog of the `codex` on `PATH` at plan time, so a dry run shows exactly what will land.
#[derive(Debug)]
pub struct CatalogPlan {
    pub path: PathBuf,
    pub models: Vec<Value>,
}

/// The interactive root page (docs/cli.md § Navigation): model, reasoning effort and
/// targets, starting from the config's `model` / `model_reasoning_effort` and the
/// group's targets for that name. Enter edits a row into the draft; Tab saves; Esc twice
/// exits, asking first when the draft differs from what is set up now.
pub fn collect_picks(args: &SetupArgs, catalog: &[CatalogModel], current: &Current) -> Result<Picks> {
    let kind = AgentKind::Codex;
    let ids = provider_ids(catalog, kind.provider());
    if ids.is_empty() {
        bail!("your account has no codex models — connect a ChatGPT subscription on the Providers page first");
    }
    if args.no_tui {
        let Some(model) = args.model.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            bail!("--model <bare id> is required with --no-tui (one of: {})", ids.join(", "));
        };
        if !ids.iter().any(|i| i == model) {
            bail!("--model {model}: not a codex model in your catalog (one of: {})", ids.join(", "));
        }
        let effort = args.effort.as_deref().unwrap_or(DEFAULT_EFFORT);
        if !EFFORTS.contains(&effort) {
            bail!("--effort {effort}: use one of {}", EFFORTS.join(", "));
        }
        let mut targets = resolve_targets(catalog, kind.provider(), &args.target).context("--target")?;
        if targets.is_empty() {
            targets.push(format!("codex/{model}"));
        }
        return Ok(Picks { model: model.to_string(), effort: effort.to_string(), targets });
    }

    tui::require_tty()?;
    let title = "kano-proxy agent setup codex";
    let options: Vec<&str> = ids.iter().map(String::as_str).collect();
    let own = |i: usize| format!("codex/{}", ids[i]);
    let default_effort = EFFORTS.iter().position(|e| *e == DEFAULT_EFFORT).unwrap_or(0);
    let mut model = current.field("model").and_then(|v| ids.iter().position(|i| *i == v)).unwrap_or(0);
    let mut effort = current
        .field("model_reasoning_effort")
        .and_then(|v| EFFORTS.iter().position(|e| *e == v))
        .unwrap_or(default_effort);
    // The group's targets for the name in place, else the model itself.
    let targets_for = |i: usize| current.targets(&ids[i]).filter(|t| !t.is_empty()).unwrap_or_else(|| vec![own(i)]);
    let candidates = target_candidates(catalog);
    let mut targets = targets_for(model);
    let baseline = current.configured.then(|| (model, effort, targets.clone()));
    let t_text = |t: &[String]| t.join(" › ");
    let mut cursor = 0usize;
    loop {
        let picks = || Picks { model: ids[model].clone(), effort: EFFORTS[effort].to_string(), targets: targets.clone() };
        let changed = |i: usize| match &baseline {
            Some(b) => [b.0 != model, b.1 != effort, b.2 != targets][i],
            None => false,
        };
        let rows = vec![
            tui::RootRow { label: "Model".into(), value: ids[model].clone(), changed: changed(0) },
            tui::RootRow { label: "Reasoning effort".into(), value: EFFORTS[effort].into(), changed: changed(1) },
            tui::RootRow { label: "Targets".into(), value: t_text(&targets), changed: changed(2) },
        ];
        let changes: Vec<String> = match &baseline {
            Some(b) => {
                let mut c = Vec::new();
                if changed(0) {
                    c.push(format!("  Model: {} → {}", ids[b.0], ids[model]));
                }
                if changed(1) {
                    c.push(format!("  Reasoning effort: {} → {}", EFFORTS[b.1], EFFORTS[effort]));
                }
                if changed(2) {
                    c.push(format!("  Targets: {} → {}", t_text(&b.2), t_text(&targets)));
                }
                c
            }
            None => vec![
                "The Codex CLI is not set up on this machine — saving sets it up with:".into(),
                format!("  Model: {}  effort: {}  targets: {}", ids[model], EFFORTS[effort], t_text(&targets)),
            ],
        };
        let status = match (&baseline, changes.len()) {
            (None, _) => "Not set up on this machine yet — Tab sets it up with these choices".to_string(),
            (Some(_), 0) => "No changes — this is what is set up now".to_string(),
            (Some(_), n) => format!("{n} unsaved change{} — Tab saves", if n == 1 { "" } else { "s" }),
        };
        let note = ["Codex sees the model name as-is, so its own catalog metadata lines up; the targets are what answers under that name."];
        let (action, at) = tui::root(title, &note, None, &rows, cursor, &status)?;
        cursor = at;
        match action {
            tui::RootAction::Save => return Ok(picks()),
            tui::RootAction::Exit => {
                if tui::exit_decision(title, &changes)? {
                    return Ok(picks());
                }
            }
            tui::RootAction::Edit(0) => {
                let note = ["Which model should Codex start with? /model inside Codex switches later."];
                if let Nav::Next(i) = tui::choose_default(title, &note, &options, 0, model)? {
                    if i != model {
                        model = i;
                        targets = targets_for(i);
                    }
                }
            }
            tui::RootAction::Edit(1) => {
                let note = ["Default reasoning effort (the proxy accepts these four; Codex's /model changes it later)."];
                if let Nav::Next(i) = tui::choose_default(title, &note, &EFFORTS, default_effort, effort)? {
                    effort = i;
                }
            }
            tui::RootAction::Edit(_) => {
                let note = [
                    &*format!("Targets behind {} — the first answers, the rest take over in order when it is benched or out of quota.", ids[model]),
                    "Space toggles (the order toggled on is the order tried) · Enter applies to the draft (nothing is saved until Tab on the main page).",
                ];
                if let Nav::Next(picked) = tui::pick_ordered(title, &note, &candidates, &targets)? {
                    // A model with nothing behind it answers as itself.
                    targets = if picked.is_empty() { vec![own(model)] } else { picked };
                }
            }
        }
    }
}

pub fn plan_from(base_url: &str, catalog: &[CatalogModel], picks: &Picks) -> Plan {
    let kind = AgentKind::Codex;
    let ids = provider_ids(catalog, kind.provider());
    let models = group_models(kind.provider(), &ids, &[(picks.model.clone(), picks.targets.clone())]);
    let names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    let base = kind.group_base(base_url);

    let catalog_plan = match bundled_catalog() {
        Ok(bundled) => Some(CatalogPlan { path: kind.config_path().with_file_name(CATALOG_FILE), models: transform_catalog(&bundled, &names) }),
        Err(e) => {
            eprintln!("· no model catalog: {e:#} — Codex runs on its fallback metadata, which is harmless");
            None
        }
    };

    let mut fields: Vec<(String, Value)> = vec![
        ("model".into(), json!(picks.model)),
        ("model_provider".into(), json!("kano")),
        ("model_reasoning_effort".into(), json!(picks.effort)),
    ];
    if catalog_plan.is_some() {
        fields.push(("model_catalog_json".into(), json!(CATALOG_FILE)));
    }
    fields.push((
        KEY_FIELD.into(),
        json!({
            "name": "Kano Proxy",
            "base_url": base,
            "experimental_bearer_token": KEY_PLACEHOLDER,
            "wire_api": "responses",
        }),
    ));
    let summary = vec![
        format!("Codex CLI → {base}"),
        format!("model: {}  effort: {}  ← {}", picks.model, picks.effort, picks.targets.join(" › ")),
    ];
    Plan { group_models: models, fields, summary, catalog: catalog_plan }
}

/// `codex debug models --bundled` as JSON, from the `codex` on `PATH`.
fn bundled_catalog() -> Result<Value> {
    let output = Command::new("codex")
        .args(["debug", "models", "--bundled"])
        .output()
        .context("no `codex` binary on PATH")?;
    if !output.status.success() {
        bail!("`codex debug models --bundled` exited with {}", output.status);
    }
    serde_json::from_slice(&output.stdout).context("`codex debug models --bundled` did not print JSON")
}

/// The docs page's `jq` transformation, in one place for every platform: keep listed models
/// the group serves, bare slugs, efforts trimmed to the proxy's four, a default inside them.
pub fn transform_catalog(bundled: &Value, served: &[String]) -> Vec<Value> {
    let Some(models) = bundled.get("models").and_then(Value::as_array) else { return Vec::new() };
    let mut out = Vec::new();
    for model in models {
        if model.get("visibility").and_then(Value::as_str) != Some("list") {
            continue;
        }
        let Some(slug) = model.get("slug").and_then(Value::as_str) else { continue };
        let bare = slug.strip_prefix("codex/").unwrap_or(slug).to_string();
        if !served.iter().any(|s| s == &bare) {
            continue;
        }
        let mut entry = model.clone();
        entry["slug"] = json!(bare);
        if let Some(levels) = entry.get_mut("supported_reasoning_levels").and_then(Value::as_array_mut) {
            levels.retain(|l| l.get("effort").and_then(Value::as_str).is_some_and(|e| EFFORTS.contains(&e)));
        }
        let default_ok = entry.get("default_reasoning_level").and_then(Value::as_str).is_some_and(|e| EFFORTS.contains(&e));
        if !default_ok {
            entry["default_reasoning_level"] = json!("medium");
        }
        out.push(entry);
    }
    out
}

pub fn write_catalog(plan: &CatalogPlan) -> Result<()> {
    let text = format!("{}\n", serde_json::to_string_pretty(&json!({ "models": plan.models }))?);
    doc::write_private(&plan.path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Current<'static> = Current { doc: None, group: None, configured: false };

    fn catalog() -> Vec<CatalogModel> {
        ["codex/gpt-5.6-sol", "codex/gpt-6-astra", "claude-code/claude-opus-5"]
            .iter()
            .map(|id| {
                let (p, u) = id.split_once('/').unwrap();
                CatalogModel { id: id.to_string(), provider: p.into(), upstream: u.into() }
            })
            .collect()
    }

    fn args(model: &str, effort: Option<&str>, target: &[&str]) -> SetupArgs {
        SetupArgs {
            no_tui: true,
            dry_run: false,
            opus: None,
            sonnet: None,
            haiku: None,
            fable: None,
            model: Some(model.into()),
            effort: effort.map(str::to_string),
            target: target.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn picks_validate_against_the_catalog_and_the_effort_ladder() {
        let p = collect_picks(&args("gpt-5.6-sol", None, &["claude-code/claude-opus-5", "gpt-6-astra"]), &catalog(), &NONE).unwrap();
        assert_eq!((p.model.as_str(), p.effort.as_str()), ("gpt-5.6-sol", "high"));
        assert_eq!(p.targets, ["claude-code/claude-opus-5", "codex/gpt-6-astra"], "targets as given, a bare id on codex");
        assert_eq!(collect_picks(&args("gpt-5.6-sol", None, &[]), &catalog(), &NONE).unwrap().targets, ["codex/gpt-5.6-sol"], "the model itself by default");
        assert!(collect_picks(&args("o3", None, &[]), &catalog(), &NONE).is_err());
        assert!(collect_picks(&args("gpt-5.6-sol", Some("minimal"), &[]), &catalog(), &NONE).is_err());
        assert!(collect_picks(&args("gpt-5.6-sol", None, &["grok/grok-4.5"]), &catalog(), &NONE).is_err());
        let no_codex: Vec<CatalogModel> = catalog().into_iter().filter(|m| m.provider != "codex").collect();
        assert!(collect_picks(&args("gpt-5.6-sol", None, &[]), &no_codex, &NONE).unwrap_err().to_string().contains("no codex models"));
    }

    #[test]
    fn the_catalog_transform_matches_the_docs_page_script() {
        let bundled = json!({ "models": [
            { "slug": "gpt-5.6-sol", "visibility": "list", "supported_reasoning_levels": [{ "effort": "minimal" }, { "effort": "low" }, { "effort": "high" }, { "effort": "ultra" }], "default_reasoning_level": "minimal", "context_window": 400000 },
            { "slug": "codex/gpt-6-astra", "visibility": "list", "supported_reasoning_levels": [{ "effort": "xhigh" }], "default_reasoning_level": "xhigh" },
            { "slug": "gpt-5-hidden", "visibility": "none", "supported_reasoning_levels": [], "default_reasoning_level": "medium" },
            { "slug": "gpt-5.6-sol-mini", "visibility": "list", "supported_reasoning_levels": [], "default_reasoning_level": "medium" }
        ]});
        let served: Vec<String> = ["gpt-5.6-sol", "gpt-6-astra"].iter().map(|s| s.to_string()).collect();
        let out = transform_catalog(&bundled, &served);
        assert_eq!(out.len(), 2, "hidden and unserved models are dropped");
        assert_eq!(out[0]["slug"], "gpt-5.6-sol");
        assert_eq!(out[0]["supported_reasoning_levels"].as_array().unwrap().len(), 2);
        assert_eq!(out[0]["default_reasoning_level"], "medium");
        assert_eq!(out[0]["context_window"], 400000, "everything else stays as Codex ships it");
        assert_eq!(out[1]["slug"], "gpt-6-astra", "a prefixed slug is bared");
        assert_eq!(out[1]["default_reasoning_level"], "xhigh");
    }
}
