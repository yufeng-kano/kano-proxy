//! `model_groups` / `model_group_models` rows (//! docs/providers.md § Model groups). A group's model set is replaced atomically, never
//! through a visible in-between state with zero or partial models.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::ids::{new_id, now_iso};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct ModelGroupRow {
    pub id: String,
    pub user_id: String,
    /// Display name (free text, unique per user) — a label, never part of the URL.
    pub name: String,
    /// The endpoint's URL id (`/g/<slug>/…`), unique per user and mutable.
    pub slug: String,
    /// Raw column value; an unrecognized strategy is normalized by `routing::strategy`,
    /// not here — this layer stores and returns what is in the row.
    pub strategy: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One callable model of a group — 1..20 per group, name unique within the group, each with
/// its own ordered target list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct ModelGroupModelRow {
    pub id: String,
    pub user_id: String,
    pub group_id: String,
    pub name: String,
    pub targets_json: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Normalized target shape: `account_id` pins the target to one `upstream_accounts` row,
/// `None` for an unpinned (whole-pool) target. No FK; a deleted account just skips at
/// resolve time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupTarget {
    pub model: String,
    pub account_id: Option<String>,
}

/// Wire/storage shape of one group model: a callable name plus its targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupModelInput {
    pub name: String,
    pub targets: Vec<GroupTarget>,
}

pub async fn list_model_groups(db: &PgPool, user_id: &str) -> Result<Vec<ModelGroupRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelGroupRow>("SELECT * FROM model_groups WHERE user_id = $1 ORDER BY created_at ASC")
        .bind(user_id)
        .fetch_all(db)
        .await
}

pub async fn get_model_group_by_id(db: &PgPool, user_id: &str, id: &str) -> Result<Option<ModelGroupRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelGroupRow>("SELECT * FROM model_groups WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .fetch_optional(db)
        .await
}

/// Resolves an endpoint slug to its group, scoped to `user_id` — a slug must never resolve
/// cross-user (docs/api.md § Group endpoints).
pub async fn get_group_by_slug(db: &PgPool, user_id: &str, slug: &str) -> Result<Option<ModelGroupRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelGroupRow>("SELECT * FROM model_groups WHERE user_id = $1 AND slug = $2")
        .bind(user_id)
        .bind(slug)
        .fetch_optional(db)
        .await
}

/// A group's models in insertion order — the order the caller submitted, which the
/// TypeScript relied on SQLite's unordered table scan for. The rows share one `created_at`
/// (they are written by a single `replace_group_models`) and the table has no sort column, so
/// physical order (`ctid`) is what reproduces it; the model list carries no priority
/// semantics (unlike each model's targets), so this is a display nicety, not correctness.
pub async fn list_models_for_group(db: &PgPool, group_id: &str) -> Result<Vec<ModelGroupModelRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelGroupModelRow>(
        "SELECT * FROM model_group_models WHERE group_id = $1 ORDER BY ctid ASC",
    )
    .bind(group_id)
    .fetch_all(db)
    .await
}

/// Resolves a request's `model` on a group endpoint: exact, case-sensitive match against the
/// group's model names. Indexed point lookup via `UNIQUE(group_id, name)`, never a JSON scan.
pub async fn get_group_model_by_name(
    db: &PgPool,
    group_id: &str,
    name: &str,
) -> Result<Option<ModelGroupModelRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelGroupModelRow>("SELECT * FROM model_group_models WHERE group_id = $1 AND name = $2")
        .bind(group_id)
        .bind(name)
        .fetch_optional(db)
        .await
}

/// Atomic replace of a group's whole model set: delete the current rows and insert the new
/// list in one transaction. Callers validate first; this function trusts its input.
pub async fn replace_group_models(
    db: &PgPool,
    user_id: &str,
    group_id: &str,
    models: &[GroupModelInput],
) -> Result<(), sqlx::Error> {
    let ts = now_iso();
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM model_group_models WHERE group_id = $1").bind(group_id).execute(&mut *tx).await?;
    for model in models {
        sqlx::query(
            "INSERT INTO model_group_models (id, user_id, group_id, name, targets_json, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $6)",
        )
        .bind(new_id("mgmodel"))
        .bind(user_id)
        .bind(group_id)
        .bind(&model.name)
        .bind(serde_json::to_string(&model.targets).expect("targets serialize"))
        .bind(&ts)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn count_model_groups(db: &PgPool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM model_groups WHERE user_id = $1").bind(user_id).fetch_one(db).await
}

pub async fn insert_model_group(
    db: &PgPool,
    user_id: &str,
    name: &str,
    slug: &str,
    strategy: Option<&str>,
) -> Result<ModelGroupRow, sqlx::Error> {
    let id = new_id("mgrp");
    let ts = now_iso();
    let strategy = strategy.unwrap_or("ordered").to_string();
    sqlx::query(
        "INSERT INTO model_groups (id, user_id, name, slug, strategy, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $6)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(name)
    .bind(slug)
    .bind(&strategy)
    .bind(&ts)
    .execute(db)
    .await?;
    Ok(ModelGroupRow {
        id,
        user_id: user_id.to_string(),
        name: name.to_string(),
        slug: slug.to_string(),
        strategy,
        created_at: ts.clone(),
        updated_at: ts,
    })
}

/// Partial update — an omitted field keeps its stored value via COALESCE.
pub async fn update_model_group_fields(
    db: &PgPool,
    id: &str,
    name: Option<&str>,
    slug: Option<&str>,
    strategy: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE model_groups SET
           name = COALESCE($1, name),
           slug = COALESCE($2, slug),
           strategy = COALESCE($3, strategy),
           updated_at = $4
         WHERE id = $5",
    )
    .bind(name)
    .bind(slug)
    .bind(strategy)
    .bind(now_iso())
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Deletes the group and its model rows in one transaction. The schema's `ON DELETE CASCADE`
/// would cover the child rows; the explicit delete keeps the behavior obvious and costs one
/// statement in the same transaction.
pub async fn delete_model_group(db: &PgPool, user_id: &str, id: &str) -> Result<bool, sqlx::Error> {
    if get_model_group_by_id(db, user_id, id).await?.is_none() {
        return Ok(false);
    }
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM model_group_models WHERE group_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM model_groups WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Tolerant parse, normalizing every entry to `{model, account_id}`:
/// a plain `"provider/model"` string (still-accepted wire shorthand) becomes an unpinned
/// target; an object reads `model` and an optional string `account_id`. Anything else is
/// dropped rather than failing, so a malformed row degrades to fewer targets instead of a 500.
pub fn parse_group_targets(json: Option<&str>) -> Vec<GroupTarget> {
    let Some(json) = json.filter(|s| !s.is_empty()) else { return Vec::new() };
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            Value::String(model) => out.push(GroupTarget { model, account_id: None }),
            Value::Object(obj) => {
                let Some(model) = obj.get("model").and_then(Value::as_str) else { continue };
                let account_id = obj
                    .get("account_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                out.push(GroupTarget { model: model.to_string(), account_id });
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool};

    #[test]
    fn targets_parse_tolerantly() {
        assert!(parse_group_targets(None).is_empty());
        assert!(parse_group_targets(Some("not json")).is_empty());
        assert!(parse_group_targets(Some(r#"{"model":"x"}"#)).is_empty(), "a non-array is no targets");
        let parsed = parse_group_targets(Some(
            r#"["codex/gpt-5", {"model":"claude-code/opus","account_id":"acc_1"}, {"model":"g/m","account_id":null}, {"model":"g/m2","account_id":""}, {"nope":1}, 7]"#,
        ));
        assert_eq!(
            parsed,
            vec![
                GroupTarget { model: "codex/gpt-5".into(), account_id: None },
                GroupTarget { model: "claude-code/opus".into(), account_id: Some("acc_1".into()) },
                GroupTarget { model: "g/m".into(), account_id: None },
                GroupTarget { model: "g/m2".into(), account_id: None },
            ]
        );
    }

    #[tokio::test]
    async fn groups_and_their_models() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "mg@example.com").await;
        let group = insert_model_group(&pool, &user.id, "Prod", "prod", None).await.unwrap();
        assert!(group.id.starts_with("mgrp_"));
        assert_eq!(group.strategy, "ordered");
        assert_eq!(count_model_groups(&pool, &user.id).await.unwrap(), 1);
        assert_eq!(get_group_by_slug(&pool, &user.id, "prod").await.unwrap().unwrap(), group);
        assert!(get_group_by_slug(&pool, "someone-else", "prod").await.unwrap().is_none());
        assert!(get_model_group_by_id(&pool, "someone-else", &group.id).await.unwrap().is_none());

        let models = vec![
            GroupModelInput { name: "fast".into(), targets: vec![GroupTarget { model: "codex/gpt-5".into(), account_id: None }] },
            GroupModelInput { name: "smart".into(), targets: vec![GroupTarget { model: "claude-code/opus".into(), account_id: Some("acc_1".into()) }] },
        ];
        replace_group_models(&pool, &user.id, &group.id, &models).await.unwrap();
        let stored = list_models_for_group(&pool, &group.id).await.unwrap();
        assert_eq!(stored.iter().map(|m| m.name.clone()).collect::<Vec<_>>(), vec!["fast", "smart"]);
        let smart = get_group_model_by_name(&pool, &group.id, "smart").await.unwrap().unwrap();
        assert_eq!(parse_group_targets(Some(&smart.targets_json))[0].account_id.as_deref(), Some("acc_1"));
        assert!(get_group_model_by_name(&pool, &group.id, "SMART").await.unwrap().is_none(), "exact, case-sensitive");

        // Replace is a whole-set swap, never additive.
        replace_group_models(&pool, &user.id, &group.id, &models[..1]).await.unwrap();
        assert_eq!(list_models_for_group(&pool, &group.id).await.unwrap().len(), 1);

        update_model_group_fields(&pool, &group.id, Some("Renamed"), None, Some("round-robin")).await.unwrap();
        let stored = get_model_group_by_id(&pool, &user.id, &group.id).await.unwrap().unwrap();
        assert_eq!(stored.name, "Renamed");
        assert_eq!(stored.slug, "prod", "an omitted field keeps its value");
        assert_eq!(stored.strategy, "round-robin");

        assert!(!delete_model_group(&pool, "someone-else", &group.id).await.unwrap());
        assert!(delete_model_group(&pool, &user.id, &group.id).await.unwrap());
        assert!(list_models_for_group(&pool, &group.id).await.unwrap().is_empty());
        assert!(list_model_groups(&pool, &user.id).await.unwrap().is_empty());
    }
}
