//! Claude Code (docs/cli.md § Command surface, the docs site's Claude Code
//! page): an `env` block in the user-scope `settings.json`, pointed at the
//! `claude-code` group's Anthropic endpoint. The three default slots are
//! written explicitly — Claude Code's own family defaults can resolve to a
//! dated id the group does not define.

use anyhow::{bail, Result};
use serde_json::{json, Value};

use super::{fallback_candidates, group_models, provider_ids, validate_fallbacks, AgentKind, Current, Plan, SetupArgs, KEY_PLACEHOLDER};
use crate::api::CatalogModel;
use crate::tui::{self, Nav};

pub const KEY_FIELD: &str = "env.ANTHROPIC_AUTH_TOKEN";

const SLOTS: [(&str, &str, &str); 3] = [
    ("Opus", "opus", "env.ANTHROPIC_DEFAULT_OPUS_MODEL"),
    ("Sonnet", "sonnet", "env.ANTHROPIC_DEFAULT_SONNET_MODEL"),
    ("Haiku", "haiku", "env.ANTHROPIC_DEFAULT_HAIKU_MODEL"),
];

#[derive(Debug, Clone)]
pub struct Picks {
    /// Per slot (Opus, Sonnet, Haiku): the bare id and its fallbacks.
    pub slots: Vec<(String, Vec<String>)>,
}

/// The first id whose name carries the family word, else the first id — the TUI's
/// pre-selection and the thing `--no-tui` must spell out.
fn default_for(ids: &[String], family: &str) -> usize {
    ids.iter().position(|id| id.to_lowercase().contains(family)).unwrap_or(0)
}

/// `resume`: the answers from a previous pass — set when the review screen sent the user
/// back, so the flow re-enters at its last step with every answer intact.
pub fn collect_picks(args: &SetupArgs, catalog: &[CatalogModel], current: &Current, resume: Option<&Picks>) -> Result<Picks> {
    let kind = AgentKind::ClaudeCode;
    let ids = provider_ids(catalog, kind.provider());
    if ids.is_empty() {
        bail!("your account has no claude-code models — connect a Claude Code subscription on the Providers page first");
    }
    if args.no_tui {
        let fallbacks = validate_fallbacks(catalog, &args.fallback)?;
        let mut slots = Vec::new();
        for ((label, _, _), flag) in SLOTS.iter().zip([&args.opus, &args.sonnet, &args.haiku]) {
            let Some(id) = flag.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                bail!("--{} <bare id> is required with --no-tui (one of: {})", label.to_lowercase(), ids.join(", "));
            };
            if !ids.iter().any(|i| i == id) {
                bail!("--{} {id}: not a claude-code model in your catalog (one of: {})", label.to_lowercase(), ids.join(", "));
            }
            slots.push((id.to_string(), fallbacks.iter().filter(|f| *f != &format!("claude-code/{id}")).cloned().collect()));
        }
        return Ok(Picks { slots });
    }

    tui::require_tty()?;
    let title = "kano-proxy agent setup claude-code";
    let options: Vec<&str> = ids.iter().map(String::as_str).collect();
    // Start from what is in place: the slot the config holds now (when it is still in the
    // catalog), else the family default; the fallbacks the group holds now. Six steps —
    // model then fallbacks per slot; Esc re-enters the previous step with its answer.
    let defaults: Vec<usize> = SLOTS.iter().map(|(_, family, _)| default_for(&ids, family)).collect();
    let mut chosen: Vec<usize> = SLOTS
        .iter()
        .zip(&defaults)
        .map(|((_, _, field), &default)| current.field(field).and_then(|v| ids.iter().position(|i| *i == v)).unwrap_or(default))
        .collect();
    let mut fallbacks: Vec<Vec<String>> =
        chosen.iter().map(|&i| current.fallbacks(&ids[i], &format!("claude-code/{}", ids[i]))).collect();
    let mut step = 0usize;
    if let Some(prev) = resume {
        for (slot, (id, fb)) in prev.slots.iter().enumerate() {
            if let Some(i) = ids.iter().position(|x| x == id) {
                chosen[slot] = i;
                fallbacks[slot] = fb.clone();
            }
        }
        step = SLOTS.len() * 2 - 1;
    }
    // A step with nothing to ask (no fallback candidates) is skipped in whichever
    // direction the user is moving, so Esc never lands on an invisible step.
    let mut backward = resume.is_some();
    while step < SLOTS.len() * 2 {
        let slot = step / 2;
        let (label, _, _) = SLOTS[slot];
        if step.is_multiple_of(2) {
            let note = [
                &*format!("Which model should Claude Code's {label} slot resolve to?"),
                "The group serves this name as-is, so Claude Code sees a real Claude id.",
            ];
            match tui::choose_default(title, &note, &options, defaults[slot], chosen[slot])? {
                Nav::Next(i) => {
                    if i != chosen[slot] {
                        chosen[slot] = i;
                        fallbacks[slot] = current.fallbacks(&ids[i], &format!("claude-code/{}", ids[i]));
                    }
                    backward = false;
                    step += 1;
                }
                Nav::Back => {
                    backward = true;
                    step = step.saturating_sub(1);
                }
            }
        } else {
            let id = &ids[chosen[slot]];
            let own = format!("claude-code/{id}");
            let candidates = fallback_candidates(catalog, &own);
            if candidates.is_empty() {
                fallbacks[slot].clear();
                if backward {
                    step -= 1;
                } else {
                    step += 1;
                }
                continue;
            }
            let note = [
                &*format!("Fallbacks for {label} ({id}) — tried in order when claude-code is benched or out of quota."),
                "Space toggles, Enter with nothing toggled means no fallback.",
            ];
            match tui::pick_many(title, &note, Some(&own), &candidates, &fallbacks[slot])? {
                Nav::Next(picked) => {
                    fallbacks[slot] = picked;
                    backward = false;
                    step += 1;
                }
                Nav::Back => {
                    backward = true;
                    step -= 1;
                }
            }
        }
    }
    Ok(Picks { slots: chosen.iter().zip(fallbacks).map(|(&i, fb)| (ids[i].clone(), fb)).collect() })
}

#[cfg(test)]
pub fn plan(base_url: &str, catalog: &[CatalogModel], args: &SetupArgs, current: &Current) -> Result<Plan> {
    Ok(plan_from(base_url, catalog, &collect_picks(args, catalog, current, None)?))
}

pub fn plan_from(base_url: &str, catalog: &[CatalogModel], picks: &Picks) -> Plan {
    let kind = AgentKind::ClaudeCode;
    let ids = provider_ids(catalog, kind.provider());
    let models = group_models(kind.provider(), &ids, &picks.slots);
    let base = kind.group_base(base_url);
    let mut fields: Vec<(String, Value)> = vec![
        ("env.ANTHROPIC_BASE_URL".into(), json!(base)),
        (KEY_FIELD.into(), json!(KEY_PLACEHOLDER)),
        ("env.ANTHROPIC_MODEL".into(), json!(picks.slots[0].0)),
    ];
    for ((_, _, field), (id, _)) in SLOTS.iter().zip(&picks.slots) {
        fields.push(((*field).into(), json!(id)));
    }
    let mut summary = vec![format!("Claude Code → {base}")];
    for ((label, _, _), (id, fallbacks)) in SLOTS.iter().zip(&picks.slots) {
        summary.push(if fallbacks.is_empty() {
            format!("{label}: {id}")
        } else {
            format!("{label}: {id}  (fallbacks: {})", fallbacks.join(", "))
        });
    }
    Plan { group_models: models, fields, summary, catalog: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Current<'static> = Current { doc: None, group: None };

    fn catalog() -> Vec<CatalogModel> {
        ["claude-code/claude-opus-5", "claude-code/claude-sonnet-5", "claude-code/claude-haiku-4-5", "codex/gpt-5.6-sol"]
            .iter()
            .map(|id| {
                let (p, u) = id.split_once('/').unwrap();
                CatalogModel { id: id.to_string(), provider: p.into(), upstream: u.into() }
            })
            .collect()
    }

    fn args(opus: &str, sonnet: &str, haiku: &str, fallback: &[&str]) -> SetupArgs {
        SetupArgs {
            no_tui: true,
            dry_run: false,
            opus: Some(opus.into()),
            sonnet: Some(sonnet.into()),
            haiku: Some(haiku.into()),
            model: None,
            effort: None,
            fallback: fallback.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_plan_writes_the_group_endpoint_and_explicit_slots() {
        let plan = plan("https://p.example", &catalog(), &args("claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5", &["codex/gpt-5.6-sol"]), &NONE).unwrap();
        let get = |f: &str| plan.fields.iter().find(|(k, _)| k == f).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(get("env.ANTHROPIC_BASE_URL"), json!("https://p.example/g/claude-code/anthropic"));
        assert_eq!(get("env.ANTHROPIC_AUTH_TOKEN"), json!(KEY_PLACEHOLDER));
        assert_eq!(get("env.ANTHROPIC_MODEL"), json!("claude-opus-5"));
        assert_eq!(get("env.ANTHROPIC_DEFAULT_HAIKU_MODEL"), json!("claude-haiku-4-5"));
        // Bare names, the agent's provider first, the fallback after, every provider id mirrored.
        assert_eq!(plan.group_models[0].name, "claude-opus-5");
        assert_eq!(plan.group_models[0].targets.iter().map(|t| t.model.as_str()).collect::<Vec<_>>(), ["claude-code/claude-opus-5", "codex/gpt-5.6-sol"]);
        assert_eq!(plan.group_models.len(), 3);
        assert!(plan.group_models.iter().all(|m| !m.name.contains('/')));
    }

    #[test]
    fn no_tui_requires_real_ids_and_a_claude_account() {
        assert!(plan("https://p.example", &catalog(), &args("claude-opus-5", "nope", "claude-haiku-4-5", &[]), &NONE).is_err());
        assert!(plan("https://p.example", &catalog(), &args("claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5", &["grok/grok-4.5"]), &NONE).is_err());
        let mut missing = args("claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5", &[]);
        missing.haiku = None;
        assert!(plan("https://p.example", &catalog(), &missing, &NONE).is_err());
        let codex_only: Vec<CatalogModel> = catalog().into_iter().filter(|m| m.provider == "codex").collect();
        let err = plan("https://p.example", &codex_only, &args("claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5", &[]), &NONE).unwrap_err();
        assert!(err.to_string().contains("no claude-code models"));
    }

    #[test]
    fn defaults_follow_the_family_name() {
        let ids: Vec<String> = ["claude-haiku-4-5", "claude-sonnet-5", "claude-opus-5"].iter().map(|s| s.to_string()).collect();
        assert_eq!(default_for(&ids, "opus"), 2);
        assert_eq!(default_for(&ids, "haiku"), 0);
        assert_eq!(default_for(&ids, "fable"), 0);
    }
}
