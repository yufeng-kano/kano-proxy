//! Claude Code (docs/cli.md § Command surface, the docs site's Claude Code
//! page): an `env` block in the user-scope `settings.json`, pointed at the
//! `claude-code` group's Anthropic endpoint. Each of Claude Code's model
//! slots — Opus, Sonnet, Haiku, Fable — is an ordered list of targets picked
//! from the whole catalog; the name the slot is served under is derived, so
//! Claude Code always sees a real Claude id whatever the targets are.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::{group_models, provider_ids, resolve_targets, target_candidates, AgentKind, Current, Plan, SetupArgs, KEY_PLACEHOLDER};
use crate::api::CatalogModel;
use crate::tui::{self, Nav};

pub const KEY_FIELD: &str = "env.ANTHROPIC_AUTH_TOKEN";
/// Claude Code's `/model` picker lists what the group endpoint serves (its `GET /v1/models`),
/// so every name in the group — not only the slots — can be chosen there.
pub const DISCOVERY_FIELD: &str = "env.CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY";

/// (label, family word, env field) — Claude Code's model slots, in picker order.
pub const SLOTS: [(&str, &str, &str); 4] = [
    ("Opus", "opus", "env.ANTHROPIC_DEFAULT_OPUS_MODEL"),
    ("Sonnet", "sonnet", "env.ANTHROPIC_DEFAULT_SONNET_MODEL"),
    ("Haiku", "haiku", "env.ANTHROPIC_DEFAULT_HAIKU_MODEL"),
    ("Fable", "fable", "env.ANTHROPIC_DEFAULT_FABLE_MODEL"),
];

#[derive(Debug, Clone)]
pub struct Picks {
    /// Per slot, in `SLOTS` order: the `provider/model` targets, first answers, the rest take
    /// over in order. Empty leaves the slot to Claude Code's own default and writes nothing.
    pub slots: Vec<Vec<String>>,
}

/// The name a slot is served under — what Claude Code sees and sends. The first target's own
/// id when it is a `claude-code` model; else this account's `claude-code` id of the slot's
/// family, so a slot routed to another provider still reports a real Claude id; else the
/// first target's upstream id as-is. `None` for a slot with no targets.
pub fn served_name(catalog: &[CatalogModel], family: &str, targets: &[String]) -> Option<String> {
    let first = targets.first()?;
    let (provider, upstream) = first.split_once('/').unwrap_or(("", first));
    if provider == AgentKind::ClaudeCode.provider() {
        return Some(upstream.to_string());
    }
    let own = provider_ids(catalog, AgentKind::ClaudeCode.provider());
    Some(own.into_iter().find(|id| id.to_lowercase().contains(family)).unwrap_or_else(|| upstream.to_string()))
}

/// What a slot starts as when nothing is in place: the account's Claude model of that family,
/// `claude-code`'s first; none when no provider serves one (the slot is then left unset).
fn default_targets(catalog: &[CatalogModel], family: &str) -> Vec<String> {
    let fits = |m: &&CatalogModel| {
        let u = m.upstream.to_lowercase();
        u.contains("claude") && u.contains(family)
    };
    catalog
        .iter()
        .filter(fits)
        .find(|m| m.provider == AgentKind::ClaudeCode.provider())
        .or_else(|| catalog.iter().find(fits))
        .map(|m| vec![m.id.clone()])
        .unwrap_or_default()
}

fn targets_text(targets: &[String]) -> String {
    if targets.is_empty() {
        "not set — Claude Code's own default".to_string()
    } else {
        targets.join(" › ")
    }
}

/// The interactive root page (docs/cli.md § Navigation): one row per slot with its targets,
/// starting from what is in place — the targets the group holds for the name the config has
/// for that slot, else the family default. Enter edits a row into the draft; Tab saves; Esc
/// twice exits, asking first when the draft differs from what is set up now.
pub fn collect_picks(args: &SetupArgs, catalog: &[CatalogModel], current: &Current) -> Result<Picks> {
    let kind = AgentKind::ClaudeCode;
    if catalog.is_empty() {
        bail!("your account has no models — connect a provider on the Providers page first");
    }
    if args.no_tui {
        let flags = [&args.opus, &args.sonnet, &args.haiku, &args.fable];
        if flags.iter().all(|f| f.is_none()) {
            bail!("--opus/--sonnet/--haiku/--fable <provider/model[,provider/model…]>: at least one slot is required with --no-tui");
        }
        let mut slots = Vec::new();
        for ((label, _, _), flag) in SLOTS.iter().zip(flags) {
            let list: Vec<String> = flag
                .as_deref()
                .map(|s| s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
                .unwrap_or_default();
            slots.push(resolve_targets(catalog, kind.provider(), &list).with_context(|| format!("--{}", label.to_lowercase()))?);
        }
        return Ok(Picks { slots });
    }

    tui::require_tty()?;
    let title = "kano-proxy agent setup claude-code";
    let candidates = target_candidates(catalog);
    let mut slots: Vec<Vec<String>> = SLOTS
        .iter()
        .map(|(_, family, field)| {
            current
                .field(field)
                .and_then(|name| current.targets(&name))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| default_targets(catalog, family))
        })
        .collect();
    // What is set up now — the draft is compared against it. None: not set up on this
    // machine yet, so saving would change everything.
    let baseline = current.configured.then(|| slots.clone());
    let mut cursor = 0usize;
    loop {
        let mut rows = Vec::new();
        let mut changes = Vec::new();
        for (slot, (label, family, _)) in SLOTS.iter().enumerate() {
            let changed = baseline.as_ref().is_some_and(|b| b[slot] != slots[slot]);
            let value = match served_name(catalog, family, &slots[slot]) {
                Some(name) => format!("{}  (as {name})", targets_text(&slots[slot])),
                None => targets_text(&slots[slot]),
            };
            rows.push(tui::RootRow { label: (*label).to_string(), value, changed });
            match &baseline {
                Some(b) => {
                    if changed {
                        changes.push(format!("  {label}: {} → {}", targets_text(&b[slot]), targets_text(&slots[slot])));
                    }
                }
                None => changes.push(format!("  {label}: {}", targets_text(&slots[slot]))),
            }
        }
        if baseline.is_none() {
            changes.insert(0, "Claude Code is not set up on this machine — saving sets it up with:".into());
        }
        let status = match (&baseline, changes.len()) {
            (None, _) => "Not set up on this machine yet — Tab sets it up with these choices".to_string(),
            (Some(_), 0) => "No changes — this is what is set up now".to_string(),
            (Some(_), n) => format!("{n} unsaved change{} — Tab saves", if n == 1 { "" } else { "s" }),
        };
        let note = ["Each slot's targets, first answers; Claude Code sees the name in parentheses, a real Claude id."];
        let (action, at) = tui::root(title, &note, &rows, cursor, &status)?;
        cursor = at;
        match action {
            tui::RootAction::Save => return Ok(Picks { slots }),
            tui::RootAction::Exit => {
                if tui::exit_decision(title, &changes)? {
                    return Ok(Picks { slots });
                }
            }
            tui::RootAction::Edit(slot) => {
                let (label, _, _) = SLOTS[slot];
                let note = [
                    &*format!("Targets for Claude Code's {label} slot — the first answers, the rest take over in order when it is benched or out of quota."),
                    "Space toggles (the order toggled on is the order tried) · none leaves the slot to Claude Code's default · Enter applies to the draft (nothing is saved until Tab on the main page).",
                ];
                if let Nav::Next(picked) = tui::pick_ordered(title, &note, &candidates, &slots[slot])? {
                    slots[slot] = picked;
                }
            }
        }
    }
}

#[cfg(test)]
pub fn plan(base_url: &str, catalog: &[CatalogModel], args: &SetupArgs, current: &Current) -> Result<Plan> {
    Ok(plan_from(base_url, catalog, &collect_picks(args, catalog, current)?))
}

pub fn plan_from(base_url: &str, catalog: &[CatalogModel], picks: &Picks) -> Plan {
    let kind = AgentKind::ClaudeCode;
    let ids = provider_ids(catalog, kind.provider());
    let named: Vec<(&str, &str, String, &Vec<String>)> = SLOTS
        .iter()
        .zip(&picks.slots)
        .filter_map(|((label, family, field), targets)| served_name(catalog, family, targets).map(|n| (*label, *field, n, targets)))
        .collect();
    let primary: Vec<(String, Vec<String>)> = named.iter().map(|(_, _, n, t)| (n.clone(), (*t).clone())).collect();
    let models = group_models(kind.provider(), &ids, &primary);
    let base = kind.group_base(base_url);
    // No `ANTHROPIC_MODEL`: it would override the default the user saves with `/model`.
    let mut fields: Vec<(String, Value)> = vec![
        ("env.ANTHROPIC_BASE_URL".into(), json!(base)),
        (KEY_FIELD.into(), json!(KEY_PLACEHOLDER)),
        (DISCOVERY_FIELD.into(), json!("1")),
    ];
    let mut summary = vec![format!("Claude Code → {base}")];
    for (label, field, name, targets) in &named {
        fields.push(((*field).into(), json!(name)));
        summary.push(format!("{label}: {name}  ← {}", targets.join(" › ")));
    }
    Plan { group_models: models, fields, summary, catalog: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Current<'static> = Current { doc: None, group: None, configured: false };

    fn catalog() -> Vec<CatalogModel> {
        [
            "claude-code/claude-opus-5",
            "claude-code/claude-sonnet-5",
            "claude-code/claude-haiku-4-5",
            "claude-code/claude-fable-5-1",
            "antigravity/claude-opus-5-high",
            "codex/gpt-5.6-sol",
        ]
        .iter()
        .map(|id| {
            let (p, u) = id.split_once('/').unwrap();
            CatalogModel { id: id.to_string(), provider: p.into(), upstream: u.into() }
        })
        .collect()
    }

    fn args(opus: Option<&str>, sonnet: Option<&str>, haiku: Option<&str>, fable: Option<&str>) -> SetupArgs {
        SetupArgs {
            no_tui: true,
            dry_run: false,
            opus: opus.map(str::to_string),
            sonnet: sonnet.map(str::to_string),
            haiku: haiku.map(str::to_string),
            fable: fable.map(str::to_string),
            model: None,
            effort: None,
            target: vec![],
        }
    }

    #[test]
    fn the_plan_writes_the_group_endpoint_and_the_slots_it_was_given() {
        let plan = plan(
            "https://p.example",
            &catalog(),
            &args(Some("antigravity/claude-opus-5-high, claude-opus-5"), Some("claude-sonnet-5"), Some("codex/gpt-5.6-sol"), None),
            &NONE,
        )
        .unwrap();
        let get = |f: &str| plan.fields.iter().find(|(k, _)| k == f).map(|(_, v)| v.clone());
        assert_eq!(get("env.ANTHROPIC_BASE_URL"), Some(json!("https://p.example/g/claude-code/anthropic")));
        assert_eq!(get("env.ANTHROPIC_AUTH_TOKEN"), Some(json!(KEY_PLACEHOLDER)));
        assert_eq!(get(DISCOVERY_FIELD), Some(json!("1")));
        assert_eq!(get("env.ANTHROPIC_MODEL"), None, "never pinned — /model's saved default must win");
        // A slot routed elsewhere first is still served under the account's Claude id of its family.
        assert_eq!(get("env.ANTHROPIC_DEFAULT_OPUS_MODEL"), Some(json!("claude-opus-5")));
        assert_eq!(get("env.ANTHROPIC_DEFAULT_HAIKU_MODEL"), Some(json!("claude-haiku-4-5")));
        assert_eq!(get("env.ANTHROPIC_DEFAULT_FABLE_MODEL"), None, "a slot without targets is left to Claude Code");
        let opus = &plan.group_models[0];
        assert_eq!(opus.name, "claude-opus-5");
        assert_eq!(opus.targets.iter().map(|t| t.model.as_str()).collect::<Vec<_>>(), ["antigravity/claude-opus-5-high", "claude-code/claude-opus-5"]);
        let haiku = plan.group_models.iter().find(|m| m.name == "claude-haiku-4-5").unwrap();
        assert_eq!(haiku.targets.iter().map(|t| t.model.as_str()).collect::<Vec<_>>(), ["codex/gpt-5.6-sol"]);
        // Every claude-code id is mirrored under its bare name.
        assert!(plan.group_models.iter().any(|m| m.name == "claude-fable-5-1"));
        assert!(plan.group_models.iter().all(|m| !m.name.contains('/')));
    }

    #[test]
    fn no_tui_requires_catalog_ids_and_at_least_one_slot() {
        assert!(plan("https://p.example", &catalog(), &args(Some("nope"), None, None, None), &NONE).is_err());
        assert!(plan("https://p.example", &catalog(), &args(Some("grok/grok-4.5"), None, None, None), &NONE).is_err());
        assert!(plan("https://p.example", &catalog(), &args(None, None, None, None), &NONE).is_err());
        assert!(plan("https://p.example", &catalog(), &args(None, None, None, Some("claude-fable-5-1")), &NONE).is_ok());
        let err = plan("https://p.example", &[], &args(Some("claude-opus-5"), None, None, None), &NONE).unwrap_err();
        assert!(err.to_string().contains("no models"));
    }

    #[test]
    fn served_names_and_defaults_follow_the_family() {
        let cat = catalog();
        let t = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(served_name(&cat, "opus", &t(&["claude-code/claude-opus-5"])).as_deref(), Some("claude-opus-5"));
        assert_eq!(served_name(&cat, "opus", &t(&["antigravity/claude-opus-5-high"])).as_deref(), Some("claude-opus-5"));
        assert_eq!(served_name(&cat, "fable", &t(&["codex/gpt-5.6-sol"])).as_deref(), Some("claude-fable-5-1"));
        assert_eq!(served_name(&cat, "opus", &[]), None);
        // No claude-code account: the first target's own id is what Claude Code sees.
        let other: Vec<CatalogModel> = cat.iter().filter(|m| m.provider != "claude-code").cloned().collect();
        assert_eq!(served_name(&other, "opus", &t(&["antigravity/claude-opus-5-high"])).as_deref(), Some("claude-opus-5-high"));
        assert_eq!(default_targets(&cat, "opus"), ["claude-code/claude-opus-5"]);
        assert_eq!(default_targets(&other, "opus"), ["antigravity/claude-opus-5-high"]);
        assert!(default_targets(&other, "haiku").is_empty());
    }
}
