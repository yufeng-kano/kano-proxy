//! Codex CLI (docs/cli.md § Command surface, the docs site's Codex CLI page):
//! a `[model_providers.kano]` table and the top-level model keys in
//! `config.toml`, pointed at the `codex-cli` group's OpenAI endpoint, plus the
//! model catalog generated from the installed `codex` binary so the `/model`
//! picker shows what this account can serve, with efforts the proxy accepts.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::{doc, fallback_candidates, group_models, provider_ids, validate_fallbacks, AgentKind, Current, Plan, SetupArgs, EFFORTS, KEY_PLACEHOLDER};
use crate::api::CatalogModel;
use crate::tui::{self, Nav};

pub const KEY_FIELD: &str = "model_providers.kano";
pub const CATALOG_FILE: &str = "kano-models.json";
const DEFAULT_EFFORT: &str = "high";

#[derive(Debug, Clone)]
pub struct Picks {
    pub model: String,
    pub effort: String,
    pub fallbacks: Vec<String>,
}

/// The catalog file `setup` writes: `~/.codex/kano-models.json`, derived from the bundled
/// catalog of the `codex` on `PATH` at plan time, so a dry run shows exactly what will land.
#[derive(Debug)]
pub struct CatalogPlan {
    pub path: PathBuf,
    pub models: Vec<Value>,
}

/// `resume`: the answers from a previous pass (the review screen sent the user back).
pub fn collect_picks(args: &SetupArgs, catalog: &[CatalogModel], current: &Current, resume: Option<&Picks>) -> Result<Picks> {
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
        let fallbacks = validate_fallbacks(catalog, &args.fallback)?
            .into_iter()
            .filter(|f| f != &format!("codex/{model}"))
            .collect();
        return Ok(Picks { model: model.to_string(), effort: effort.to_string(), fallbacks });
    }

    tui::require_tty()?;
    let title = "kano-proxy agent setup codex";
    let options: Vec<&str> = ids.iter().map(String::as_str).collect();
    // Start from what is in place: the config's model and effort when still valid, the
    // group's fallbacks. Three steps; Esc re-enters the previous one with its answer.
    let default_effort = EFFORTS.iter().position(|e| *e == DEFAULT_EFFORT).unwrap_or(0);
    let mut model = current.field("model").and_then(|v| ids.iter().position(|i| *i == v)).unwrap_or(0);
    let mut effort = current
        .field("model_reasoning_effort")
        .and_then(|v| EFFORTS.iter().position(|e| *e == v))
        .unwrap_or(default_effort);
    let mut fallbacks = current.fallbacks(&ids[model], &format!("codex/{}", ids[model]));
    let mut step = 0usize;
    if let Some(prev) = resume {
        if let Some(i) = ids.iter().position(|x| *x == prev.model) {
            model = i;
        }
        if let Some(i) = EFFORTS.iter().position(|e| *e == prev.effort) {
            effort = i;
        }
        fallbacks = prev.fallbacks.clone();
        step = 2;
    }
    let mut backward = resume.is_some();
    loop {
        match step {
            0 => {
                let note = ["Which model should Codex start with? /model inside Codex switches later.", "The group serves this name as-is, so Codex's own catalog metadata lines up."];
                match tui::choose_default(title, &note, &options, 0, model)? {
                    Nav::Next(i) => {
                        if i != model {
                            model = i;
                            fallbacks = current.fallbacks(&ids[i], &format!("codex/{}", ids[i]));
                        }
                        backward = false;
                        step = 1;
                    }
                    Nav::Back => {}
                }
            }
            1 => {
                let note = ["Default reasoning effort (the proxy accepts these four; Codex's /model changes it later)."];
                match tui::choose_default(title, &note, &EFFORTS, default_effort, effort)? {
                    Nav::Next(i) => {
                        effort = i;
                        backward = false;
                        step = 2;
                    }
                    Nav::Back => {
                        backward = true;
                        step = 0;
                    }
                }
            }
            _ => {
                let own = format!("codex/{}", ids[model]);
                let candidates = fallback_candidates(catalog, &own);
                if candidates.is_empty() {
                    fallbacks.clear();
                    if backward {
                        step = 1;
                        continue;
                    }
                    break;
                }
                let note = [
                    &*format!("Fallbacks for {} — tried in order when codex is benched or out of quota.", ids[model]),
                    "Space toggles, Enter with nothing toggled means no fallback.",
                ];
                match tui::pick_many(title, &note, Some(&own), &candidates, &fallbacks)? {
                    Nav::Next(picked) => {
                        fallbacks = picked;
                        break;
                    }
                    Nav::Back => {
                        backward = true;
                        step = 1;
                    }
                }
            }
        }
    }
    Ok(Picks { model: ids[model].clone(), effort: EFFORTS[effort].to_string(), fallbacks })
}

pub fn plan_from(base_url: &str, catalog: &[CatalogModel], picks: &Picks) -> Plan {
    let kind = AgentKind::Codex;
    let ids = provider_ids(catalog, kind.provider());
    let models = group_models(kind.provider(), &ids, &[(picks.model.clone(), picks.fallbacks.clone())]);
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
    let mut summary = vec![format!("Codex CLI → {base}"), format!("model: {}  effort: {}", picks.model, picks.effort)];
    if !picks.fallbacks.is_empty() {
        summary.push(format!("fallbacks: {}", picks.fallbacks.join(", ")));
    }
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

    const NONE: Current<'static> = Current { doc: None, group: None };

    fn catalog() -> Vec<CatalogModel> {
        ["codex/gpt-5.6-sol", "codex/gpt-6-astra", "claude-code/claude-opus-5"]
            .iter()
            .map(|id| {
                let (p, u) = id.split_once('/').unwrap();
                CatalogModel { id: id.to_string(), provider: p.into(), upstream: u.into() }
            })
            .collect()
    }

    fn args(model: &str, effort: Option<&str>, fallback: &[&str]) -> SetupArgs {
        SetupArgs {
            no_tui: true,
            dry_run: false,
            opus: None,
            sonnet: None,
            haiku: None,
            model: Some(model.into()),
            effort: effort.map(str::to_string),
            fallback: fallback.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn picks_validate_against_the_catalog_and_the_effort_ladder() {
        let p = collect_picks(&args("gpt-5.6-sol", None, &["claude-code/claude-opus-5"]), &catalog(), &NONE, None).unwrap();
        assert_eq!((p.model.as_str(), p.effort.as_str()), ("gpt-5.6-sol", "high"));
        assert_eq!(p.fallbacks, ["claude-code/claude-opus-5"]);
        assert!(collect_picks(&args("o3", None, &[]), &catalog(), &NONE, None).is_err());
        assert!(collect_picks(&args("gpt-5.6-sol", Some("minimal"), &[]), &catalog(), &NONE, None).is_err());
        assert!(collect_picks(&args("gpt-5.6-sol", None, &["grok/grok-4.5"]), &catalog(), &NONE, None).is_err());
        let no_codex: Vec<CatalogModel> = catalog().into_iter().filter(|m| m.provider != "codex").collect();
        assert!(collect_picks(&args("gpt-5.6-sol", None, &[]), &no_codex, &NONE, None).unwrap_err().to_string().contains("no codex models"));
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
