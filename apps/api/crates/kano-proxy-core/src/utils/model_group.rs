//! Validation and limits for user-defined model groups (//! docs/providers.md § Model groups).

use std::future::Future;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::split_model_id;

/// `ordered`. Kept here until the
/// routing module's port lands; see the port report.
pub const DEFAULT_STRATEGY: &str = "ordered";

pub const MAX_MODEL_GROUPS_PER_USER: usize = 50;
pub const MAX_MODELS_PER_GROUP: usize = 20;
pub const MAX_TARGETS_PER_MODEL: usize = 20;
pub const MAX_MODEL_NAME_LENGTH: usize = 128;
pub const MAX_DISPLAY_NAME_LENGTH: usize = 64;

/// One routing target: `provider/model` plus an optional pinned `upstream_accounts` id
///.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupTarget {
    pub model: String,
    pub account_id: Option<String>,
}

/// Wire/storage shape of one group model: a callable name plus its targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupModelInput {
    pub name: String,
    pub targets: Vec<GroupTarget>,
}

/// Same shape as a custom-provider slug (docs/providers.md § Model groups "Slug") — but with
/// **no reserved-word list**: the `/g/` path prefix is its own namespace, so a group slug can
/// never collide with a provider id or any other route.
static GROUP_SLUG_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$").expect("group slug regex"));

pub fn validate_group_slug(slug: &str) -> Option<String> {
    let len = slug.chars().count();
    if !(2..=32).contains(&len) {
        return Some("slug must be 2-32 characters".into());
    }
    if !GROUP_SLUG_RE.is_match(slug) {
        return Some(
            "slug must be lowercase alphanumeric with hyphens, starting and ending with a letter or digit"
                .into(),
        );
    }
    None
}

/// One callable model name on a group endpoint: trimmed, 1-128 chars, no whitespace. Unlike
/// the v3 bare-name aliases, `/` **is allowed** — a group endpoint has no `provider/model`
/// resolution to collide with, so a group can mirror full ids as well as bare ones.
pub fn validate_model_name(name: &str) -> Option<String> {
    if name.is_empty() || name.chars().count() > MAX_MODEL_NAME_LENGTH {
        return Some(format!("model name must be 1-{MAX_MODEL_NAME_LENGTH} characters"));
    }
    if name.chars().any(char::is_whitespace) {
        return Some("model name must not contain whitespace".into());
    }
    None
}

/// A group's display name: trimmed, 1-64 chars, free text (spaces fine — a label, never part
/// of the URL; the callable surface is the slug + models).
pub fn validate_display_name(name: &str) -> Option<String> {
    if name.is_empty() || name.chars().count() > MAX_DISPLAY_NAME_LENGTH {
        return Some(format!("name must be 1-{MAX_DISPLAY_NAME_LENGTH} characters"));
    }
    None
}

/// `strategy` (docs/providers.md § Routing module): `ordered` is the only accepted value
/// today, so this is a strict equality check, not a set membership one. An omitted field is
/// the caller's job to default, not this validator's — see the POST/PUT routes.
pub fn validate_strategy(strategy: &Value) -> Option<String> {
    if strategy.as_str() != Some(DEFAULT_STRATEGY) {
        return Some(format!("strategy must be \"{DEFAULT_STRATEGY}\""));
    }
    None
}

/// `resolve_prefix` decides whether a target's `model` prefix is a valid builtin provider id
/// or one of the caller's own custom slugs — sync, no DB. `resolve_account` decides whether a
/// pinned `account_id` is an `upstream_accounts` row owned by the caller whose `provider`
/// matches the target's prefix (docs/auth.md § Model groups) — async and DB-backed, so (like
/// `resolve_prefix`) it is injected rather than queried here; this module stays free of DB
/// access.
///
/// Each target entry may be a bare `"provider/model"` string (shorthand for `{model}`, still
/// accepted on the wire and in storage) or an object `{model, account_id?}`. Duplicate
/// identity is `model` + `account_id` together, so the same model pinned to two different
/// accounts (or once pinned and once not) is two legitimate targets.
pub async fn validate_group_targets<P, A, Fut>(
    targets: &Value,
    resolve_prefix: &P,
    resolve_account: &A,
) -> Result<Vec<GroupTarget>, String>
where
    P: Fn(&str) -> bool,
    A: Fn(String, String) -> Fut,
    Fut: Future<Output = bool>,
{
    let entries = match targets.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return Err("targets must be a non-empty array".into()),
    };
    if entries.len() > MAX_TARGETS_PER_MODEL {
        return Err(format!("targets must have at most {MAX_TARGETS_PER_MODEL} entries"));
    }
    let mut out: Vec<GroupTarget> = Vec::with_capacity(entries.len());
    let mut seen: Vec<String> = Vec::with_capacity(entries.len());
    for t in entries {
        let (model_raw, account_id_raw): (Option<&Value>, Option<&Value>) = match t {
            Value::String(_) => (Some(t), None),
            Value::Object(obj) => (obj.get("model"), obj.get("account_id")),
            _ => {
                return Err("targets entries must be a string or a {model, account_id?} object".into())
            }
        };

        let Some(model) = model_raw.and_then(Value::as_str) else {
            return Err("target model must be a string".into());
        };
        let trimmed = model.trim().to_string();
        let Some(split) = split_model_id(&trimmed) else {
            return Err(format!("target \"{trimmed}\" must be provider/model"));
        };
        // A bare name (no slash) is rejected above by split_model_id already, but spell it
        // out: a group can never target another group's model — no nesting, no cycles,
        // structurally.
        if !resolve_prefix(&split.prefix) {
            return Err(format!(
                "target \"{trimmed}\" has an unknown provider \"{}\"",
                split.prefix
            ));
        }

        let mut account_id: Option<String> = None;
        match account_id_raw {
            None | Some(Value::Null) => {}
            Some(v) => {
                let id = match v.as_str() {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => return Err(format!("target \"{trimmed}\" has an invalid account_id")),
                };
                if !resolve_account(id.clone(), split.prefix.clone()).await {
                    return Err(format!(
                        "target \"{trimmed}\" account_id does not belong to this user's \"{}\" provider",
                        split.prefix
                    ));
                }
                account_id = Some(id);
            }
        }

        // A NUL separator (never legal inside a target string) rules out any collision
        // between two distinct (model, account_id) pairs.
        let identity = format!("{trimmed}\u{0}{}", account_id.as_deref().unwrap_or(""));
        if seen.iter().any(|s| s == &identity) {
            return Err(match &account_id {
                Some(id) => format!("duplicate target \"{trimmed}\" pinned to account \"{id}\""),
                None => format!("duplicate target \"{trimmed}\""),
            });
        }
        seen.push(identity);
        out.push(GroupTarget { model: trimmed, account_id });
    }
    Ok(out)
}

/// The group's whole model set: 1-20 entries of `{name, targets}`, names unique **within the
/// payload** (= within the group — the set replaces the stored one wholesale, and other
/// groups may reuse a name freely since the endpoint is the namespace). Each entry's targets
/// go through [`validate_group_targets`], with the model's name prefixed onto any error so
/// the message says which model it is about.
pub async fn validate_group_models<P, A, Fut>(
    models: &Value,
    resolve_prefix: &P,
    resolve_account: &A,
) -> Result<Vec<GroupModelInput>, String>
where
    P: Fn(&str) -> bool,
    A: Fn(String, String) -> Fut,
    Fut: Future<Output = bool>,
{
    let entries = match models.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return Err("models must be a non-empty array".into()),
    };
    if entries.len() > MAX_MODELS_PER_GROUP {
        return Err(format!("models must have at most {MAX_MODELS_PER_GROUP} entries"));
    }
    let mut out: Vec<GroupModelInput> = Vec::with_capacity(entries.len());
    let mut seen: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(obj) = entry.as_object() else {
            return Err("models entries must be {name, targets} objects".into());
        };
        let Some(name_raw) = obj.get("name").and_then(Value::as_str) else {
            return Err("model name must be a string".into());
        };
        let name = name_raw.trim().to_string();
        if let Some(err) = validate_model_name(&name) {
            return Err(err);
        }
        if seen.iter().any(|s| s == &name) {
            return Err(format!("duplicate model name \"{name}\""));
        }
        seen.push(name.clone());

        let targets_value = obj.get("targets").cloned().unwrap_or(Value::Null);
        match validate_group_targets(&targets_value, resolve_prefix, resolve_account).await {
            Ok(targets) => out.push(GroupModelInput { name, targets }),
            Err(error) => return Err(format!("model \"{name}\": {error}")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    fn builtin_only(prefix: &str) -> bool {
        matches!(prefix, "claude-code" | "grok")
    }

    fn builtin_or_codex(prefix: &str) -> bool {
        matches!(prefix, "claude-code" | "grok" | "codex")
    }

    async fn no_accounts(_id: String, _provider: String) -> bool {
        false
    }

    async fn all_accounts(_id: String, _provider: String) -> bool {
        true
    }

    fn target(model: &str, account_id: Option<&str>) -> GroupTarget {
        GroupTarget { model: model.into(), account_id: account_id.map(str::to_string) }
    }

    #[test]
    fn model_name_accepts_bare_names_slashes_and_punctuation() {
        assert_eq!(validate_model_name("opus"), None);
        assert_eq!(validate_model_name(&"a".repeat(MAX_MODEL_NAME_LENGTH)), None);
        assert_eq!(validate_model_name("claude-code/claude-opus-5"), None);
        assert_eq!(validate_model_name("a/b"), None);
        assert_eq!(validate_model_name("gpt-4o"), None);
        assert_eq!(validate_model_name("my_group.v2"), None);
    }

    #[test]
    fn model_name_rejects_empty_overlong_and_whitespace() {
        assert!(validate_model_name("").is_some());
        assert!(validate_model_name(&"a".repeat(MAX_MODEL_NAME_LENGTH + 1)).is_some());
        assert!(validate_model_name("my model").is_some());
        assert!(validate_model_name(" opus").is_some());
        assert!(validate_model_name("opus ").is_some());
    }

    #[test]
    fn group_slug_shapes() {
        assert_eq!(validate_group_slug("my-tools"), None);
        for bad in ["a", &"a".repeat(33), "-lead", "trail-", "UPPER", "has space"] {
            assert!(validate_group_slug(bad).is_some(), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn group_slug_has_no_reserved_word_list() {
        assert_eq!(validate_group_slug("openai"), None);
        assert_eq!(validate_group_slug("api"), None);
        assert_eq!(validate_group_slug("claude-code"), None);
    }

    #[test]
    fn display_name_is_free_text_within_64_chars() {
        assert_eq!(validate_display_name("Opus"), None);
        assert_eq!(validate_display_name("OpenAI GPT-4o family"), None);
        assert_eq!(validate_display_name(&"a".repeat(MAX_DISPLAY_NAME_LENGTH)), None);
        assert_eq!(validate_display_name("GPT-4o / GPT-4 family"), None);
        assert!(validate_display_name("").is_some());
        assert!(validate_display_name(&"a".repeat(MAX_DISPLAY_NAME_LENGTH + 1)).is_some());
    }

    #[test]
    fn strategy_accepts_only_ordered() {
        assert_eq!(validate_strategy(&json!("ordered")), None);
        assert_eq!(validate_strategy(&json!("random")).as_deref(), Some("strategy must be \"ordered\""));
        assert!(validate_strategy(&Value::Null).is_some());
    }

    #[tokio::test]
    async fn models_accept_a_single_entry_and_trim_the_name() {
        let res = validate_group_models(
            &json!([{ "name": " gpt-4o ", "targets": ["claude-code/claude-opus-5"] }]),
            &builtin_only,
            &no_accounts,
        )
        .await;
        assert_eq!(
            res,
            Ok(vec![GroupModelInput {
                name: "gpt-4o".into(),
                targets: vec![target("claude-code/claude-opus-5", None)],
            }])
        );
    }

    #[tokio::test]
    async fn models_accept_up_to_the_max_count() {
        let models: Vec<Value> = (0..MAX_MODELS_PER_GROUP)
            .map(|i| json!({ "name": format!("model-{i}"), "targets": ["grok/grok-4.5"] }))
            .collect();
        assert!(validate_group_models(&json!(models), &builtin_only, &no_accounts).await.is_ok());
    }

    #[tokio::test]
    async fn models_reject_empty_non_array_and_above_max_counts() {
        assert!(validate_group_models(&json!([]), &builtin_only, &no_accounts).await.is_err());
        assert!(validate_group_models(&json!("gpt-4o"), &builtin_only, &no_accounts).await.is_err());
        let models: Vec<Value> = (0..MAX_MODELS_PER_GROUP + 1)
            .map(|i| json!({ "name": format!("model-{i}"), "targets": ["grok/grok-4.5"] }))
            .collect();
        assert!(validate_group_models(&json!(models), &builtin_only, &no_accounts).await.is_err());
    }

    #[tokio::test]
    async fn models_reject_a_non_object_entry_and_a_non_string_name() {
        assert!(validate_group_models(&json!(["gpt-4o"]), &builtin_only, &no_accounts).await.is_err());
        assert!(validate_group_models(
            &json!([{ "name": 42, "targets": ["grok/grok-4.5"] }]),
            &builtin_only,
            &no_accounts
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn models_reject_an_in_payload_duplicate_name_but_case_distinguishes() {
        let dup = validate_group_models(
            &json!([
                { "name": "gpt-4o", "targets": ["grok/grok-4.5"] },
                { "name": " gpt-4o ", "targets": ["claude-code/claude-opus-5"] },
            ]),
            &builtin_only,
            &no_accounts,
        )
        .await;
        assert!(dup.is_err());
        let cased = validate_group_models(
            &json!([
                { "name": "GPT-4o", "targets": ["grok/grok-4.5"] },
                { "name": "gpt-4o", "targets": ["claude-code/claude-opus-5"] },
            ]),
            &builtin_only,
            &no_accounts,
        )
        .await;
        assert!(cased.is_ok());
    }

    #[tokio::test]
    async fn a_models_target_error_is_prefixed_with_the_model_name() {
        let res = validate_group_models(
            &json!([{ "name": "gpt-4o", "targets": [] }]),
            &builtin_only,
            &no_accounts,
        )
        .await;
        let err = res.expect_err("empty targets rejected");
        assert!(err.contains("model \"gpt-4o\""), "{err}");
    }

    #[tokio::test]
    async fn a_slash_carrying_model_name_is_legal() {
        let res = validate_group_models(
            &json!([{ "name": "claude-code/claude-opus-5", "targets": ["grok/grok-4.5"] }]),
            &builtin_only,
            &no_accounts,
        )
        .await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn targets_accept_the_string_shorthand_and_the_bare_object() {
        assert_eq!(
            validate_group_targets(&json!(["claude-code/claude-opus-5"]), &builtin_or_codex, &no_accounts).await,
            Ok(vec![target("claude-code/claude-opus-5", None)])
        );
        assert_eq!(
            validate_group_targets(&json!([{ "model": "claude-code/claude-opus-5" }]), &builtin_or_codex, &no_accounts)
                .await,
            Ok(vec![target("claude-code/claude-opus-5", None)])
        );
    }

    #[tokio::test]
    async fn targets_count_bounds() {
        let ok: Vec<Value> = (0..MAX_TARGETS_PER_MODEL).map(|i| json!(format!("claude-code/model-{i}"))).collect();
        assert!(validate_group_targets(&json!(ok), &builtin_or_codex, &no_accounts).await.is_ok());
        let too_many: Vec<Value> =
            (0..MAX_TARGETS_PER_MODEL + 1).map(|i| json!(format!("claude-code/model-{i}"))).collect();
        assert!(validate_group_targets(&json!(too_many), &builtin_or_codex, &no_accounts).await.is_err());
        assert!(validate_group_targets(&json!([]), &builtin_or_codex, &no_accounts).await.is_err());
        assert!(validate_group_targets(&json!("claude-code/claude-opus-5"), &builtin_or_codex, &no_accounts)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn targets_reject_unknown_prefixes_bare_names_duplicates_and_bad_entries() {
        assert!(validate_group_targets(&json!(["not-a-real-provider/model"]), &builtin_or_codex, &no_accounts)
            .await
            .is_err());
        assert!(validate_group_targets(&json!(["some-other-group"]), &builtin_or_codex, &no_accounts).await.is_err());
        assert!(validate_group_targets(
            &json!(["claude-code/claude-opus-5", "claude-code/claude-opus-5"]),
            &builtin_or_codex,
            &no_accounts
        )
        .await
        .is_err());
        assert!(validate_group_targets(&json!([42]), &builtin_or_codex, &no_accounts).await.is_err());
        assert!(validate_group_targets(&json!([{ "model": 42 }]), &builtin_or_codex, &no_accounts).await.is_err());
    }

    #[tokio::test]
    async fn targets_accept_a_custom_slug_prefix_and_trim_each_string() {
        let custom = |prefix: &str| prefix == "my-endpoint";
        assert_eq!(
            validate_group_targets(&json!(["my-endpoint/gpt-4o"]), &custom, &no_accounts).await,
            Ok(vec![target("my-endpoint/gpt-4o", None)])
        );
        assert_eq!(
            validate_group_targets(&json!([" claude-code/claude-opus-5 "]), &builtin_or_codex, &no_accounts).await,
            Ok(vec![target("claude-code/claude-opus-5", None)])
        );
    }

    #[tokio::test]
    async fn pinning_accepts_an_approved_account_and_rejects_a_foreign_one() {
        assert_eq!(
            validate_group_targets(
                &json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_1" }]),
                &builtin_or_codex,
                &all_accounts
            )
            .await,
            Ok(vec![target("claude-code/claude-opus-5", Some("acc_1"))])
        );
        assert!(validate_group_targets(
            &json!([{ "model": "claude-code/claude-opus-5", "account_id": "acc_1" }]),
            &builtin_or_codex,
            &no_accounts
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn the_account_resolver_sees_each_targets_own_prefix() {
        let seen: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
        {
            let record = |id: String, provider: String| {
                seen.borrow_mut().push((id, provider));
                async { true }
            };
            let _ = validate_group_targets(
                &json!([
                    { "model": "claude-code/claude-opus-5", "account_id": "acc_cc" },
                    { "model": "grok/grok-4.5", "account_id": "acc_grok" },
                ]),
                &builtin_or_codex,
                &record,
            )
            .await;
        }
        assert_eq!(
            seen.into_inner(),
            vec![
                ("acc_cc".to_string(), "claude-code".to_string()),
                ("acc_grok".to_string(), "grok".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn pinning_rejects_a_non_string_and_an_empty_account_id() {
        assert!(validate_group_targets(
            &json!([{ "model": "claude-code/claude-opus-5", "account_id": 42 }]),
            &builtin_or_codex,
            &all_accounts
        )
        .await
        .is_err());
        assert!(validate_group_targets(
            &json!([{ "model": "claude-code/claude-opus-5", "account_id": "" }]),
            &builtin_or_codex,
            &all_accounts
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn a_null_account_id_is_the_same_as_omitted() {
        assert_eq!(
            validate_group_targets(
                &json!([{ "model": "claude-code/claude-opus-5", "account_id": null }]),
                &builtin_or_codex,
                &all_accounts
            )
            .await,
            Ok(vec![target("claude-code/claude-opus-5", None)])
        );
    }

    #[tokio::test]
    async fn identity_is_model_plus_account_id() {
        let two_accounts = validate_group_targets(
            &json!([
                { "model": "claude-code/claude-opus-5", "account_id": "acc_1" },
                { "model": "claude-code/claude-opus-5", "account_id": "acc_2" },
            ]),
            &builtin_or_codex,
            &all_accounts,
        )
        .await
        .expect("two distinct pins");
        assert_eq!(two_accounts.len(), 2);

        let pinned_and_not = validate_group_targets(
            &json!([
                { "model": "claude-code/claude-opus-5", "account_id": "acc_1" },
                { "model": "claude-code/claude-opus-5" },
            ]),
            &builtin_or_codex,
            &all_accounts,
        )
        .await
        .expect("one pinned, one not");
        assert_eq!(pinned_and_not.len(), 2);

        assert!(validate_group_targets(
            &json!([
                { "model": "claude-code/claude-opus-5", "account_id": "acc_1" },
                { "model": "claude-code/claude-opus-5", "account_id": "acc_1" },
            ]),
            &builtin_or_codex,
            &all_accounts
        )
        .await
        .is_err());
        assert!(validate_group_targets(
            &json!([{ "model": "claude-code/claude-opus-5" }, "claude-code/claude-opus-5"]),
            &builtin_or_codex,
            &all_accounts
        )
        .await
        .is_err());
    }

    #[test]
    fn documented_limits() {
        assert_eq!(MAX_MODEL_GROUPS_PER_USER, 50);
        assert_eq!(MAX_TARGETS_PER_MODEL, 20);
        assert_eq!(MAX_MODELS_PER_GROUP, 20);
    }
}
