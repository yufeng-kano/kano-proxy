//! `custom_providers` rows. The slug namespace is
//! shared with `cli_providers` (docs/cli.md § Data model), so the cross-table guard and the
//! per-user cap ride inside the INSERT rather than a check-then-insert pair.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::ids::{new_id, now_iso};

/// Combined cap across custom and CLI providers.
pub const MAX_CUSTOM_PROVIDERS_PER_USER: i64 = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct CustomProviderRow {
    pub id: String,
    pub user_id: String,
    pub slug: String,
    pub name: String,
    /// `openai` or `anthropic`.
    pub format: String,
    pub base_url: String,
    pub count_tokens_url: Option<String>,
    /// `auto` or `manual`.
    pub models_mode: String,
    pub manual_models_json: Option<String>,
    pub sort_order: i32,
    pub created_at: String,
    pub updated_at: String,
}

pub async fn list_custom_providers(db: &PgPool, user_id: &str) -> Result<Vec<CustomProviderRow>, sqlx::Error> {
    sqlx::query_as::<_, CustomProviderRow>(
        "SELECT * FROM custom_providers WHERE user_id = $1 ORDER BY sort_order ASC, created_at ASC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

pub async fn get_custom_provider_by_id(
    db: &PgPool,
    user_id: &str,
    id: &str,
) -> Result<Option<CustomProviderRow>, sqlx::Error> {
    sqlx::query_as::<_, CustomProviderRow>("SELECT * FROM custom_providers WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .fetch_optional(db)
        .await
}

/// Scoped to `user_id` — a custom slug must never resolve cross-user.
pub async fn get_custom_provider_by_slug(
    db: &PgPool,
    user_id: &str,
    slug: &str,
) -> Result<Option<CustomProviderRow>, sqlx::Error> {
    sqlx::query_as::<_, CustomProviderRow>("SELECT * FROM custom_providers WHERE user_id = $1 AND slug = $2")
        .bind(user_id)
        .bind(slug)
        .fetch_optional(db)
        .await
}

pub async fn count_custom_providers(db: &PgPool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM custom_providers WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
}

#[derive(Debug, Clone)]
pub struct NewCustomProvider<'a> {
    pub user_id: &'a str,
    pub slug: &'a str,
    pub name: &'a str,
    pub format: &'a str,
    pub base_url: &'a str,
    pub count_tokens_url: Option<&'a str>,
    pub models_mode: &'a str,
    pub manual_models_json: Option<&'a str>,
}

/// `None` means the slug is taken by a CLI provider or the combined cap is reached — the
/// guard rides inside the INSERT so it cannot race a concurrent CLI create for the same slug.
pub async fn insert_custom_provider(
    db: &PgPool,
    input: NewCustomProvider<'_>,
) -> Result<Option<CustomProviderRow>, sqlx::Error> {
    let id = new_id("cprov");
    let ts = now_iso();
    let count = count_custom_providers(db, input.user_id).await?;
    let max: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), 0) FROM custom_providers WHERE user_id = $1")
        .bind(input.user_id)
        .fetch_one(db)
        .await?;
    let sort_order = if count > 0 { max.max((count - 1) as i32) + 1 } else { 0 };
    let r = sqlx::query(
        "INSERT INTO custom_providers
         (id, user_id, slug, name, format, base_url, count_tokens_url, models_mode, manual_models_json, sort_order, created_at, updated_at)
         SELECT $1::text, $2::text, $3::text, $4::text, $5::text, $6::text, $7::text, $8::text,
                $9::text, $10::int, $11::text, $11::text
         WHERE NOT EXISTS (SELECT 1 FROM cli_providers WHERE user_id = $2 AND slug = $3)
           AND ((SELECT COUNT(*) FROM cli_providers WHERE user_id = $2)
              + (SELECT COUNT(*) FROM custom_providers WHERE user_id = $2)) < $12::bigint",
    )
    .bind(&id)
    .bind(input.user_id)
    .bind(input.slug)
    .bind(input.name)
    .bind(input.format)
    .bind(input.base_url)
    .bind(input.count_tokens_url)
    .bind(input.models_mode)
    .bind(input.manual_models_json)
    .bind(sort_order)
    .bind(&ts)
    .bind(MAX_CUSTOM_PROVIDERS_PER_USER)
    .execute(db)
    .await?;
    if r.rows_affected() == 0 {
        return Ok(None);
    }
    Ok(Some(CustomProviderRow {
        id,
        user_id: input.user_id.to_string(),
        slug: input.slug.to_string(),
        name: input.name.to_string(),
        format: input.format.to_string(),
        base_url: input.base_url.to_string(),
        count_tokens_url: input.count_tokens_url.map(str::to_string),
        models_mode: input.models_mode.to_string(),
        manual_models_json: input.manual_models_json.map(str::to_string),
        sort_order,
        created_at: ts.clone(),
        updated_at: ts,
    }))
}

/// Renumbers a user's whole custom-provider list in one transaction (the D1 batch).
pub async fn reorder_custom_providers(db: &PgPool, user_id: &str, ordered_ids: &[String]) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    for (sort_order, id) in ordered_ids.iter().enumerate() {
        sqlx::query("UPDATE custom_providers SET sort_order = $1, updated_at = $2 WHERE id = $3 AND user_id = $4")
            .bind(sort_order as i32)
            .bind(now_iso())
            .bind(id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// Partial update: an omitted field keeps its stored value via COALESCE. To clear
/// `manual_models_json`, pass `Some("[]")`. `count_tokens_url` is nullable *and* clearable,
/// which COALESCE cannot express, so `Some(value)` (including `Some(None)`) gets its own
/// direct assignment and `None` means "the caller did not send the field".
#[derive(Debug, Clone, Default)]
pub struct CustomProviderPatch<'a> {
    pub name: Option<&'a str>,
    pub base_url: Option<&'a str>,
    pub count_tokens_url: Option<Option<&'a str>>,
    pub models_mode: Option<&'a str>,
    pub manual_models_json: Option<&'a str>,
}

pub async fn update_custom_provider_fields(
    db: &PgPool,
    id: &str,
    patch: CustomProviderPatch<'_>,
) -> Result<(), sqlx::Error> {
    let ts = now_iso();
    sqlx::query(
        "UPDATE custom_providers SET
           name = COALESCE($1, name),
           base_url = COALESCE($2, base_url),
           models_mode = COALESCE($3, models_mode),
           manual_models_json = COALESCE($4, manual_models_json),
           updated_at = $5
         WHERE id = $6",
    )
    .bind(patch.name)
    .bind(patch.base_url)
    .bind(patch.models_mode)
    .bind(patch.manual_models_json)
    .bind(&ts)
    .bind(id)
    .execute(db)
    .await?;

    if let Some(count_tokens_url) = patch.count_tokens_url {
        sqlx::query("UPDATE custom_providers SET count_tokens_url = $1, updated_at = $2 WHERE id = $3")
            .bind(count_tokens_url)
            .bind(&ts)
            .bind(id)
            .execute(db)
            .await?;
    }
    Ok(())
}

pub async fn delete_custom_provider(db: &PgPool, user_id: &str, id: &str) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("DELETE FROM custom_providers WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_user, skip_without_db, test_pool};

    fn input<'a>(user_id: &'a str, slug: &'a str) -> NewCustomProvider<'a> {
        NewCustomProvider {
            user_id,
            slug,
            name: "My endpoint",
            format: "openai",
            base_url: "https://example.test/v1",
            count_tokens_url: None,
            models_mode: "auto",
            manual_models_json: None,
        }
    }

    #[tokio::test]
    async fn insert_read_patch_delete() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "cp@example.com").await;
        let row = insert_custom_provider(&pool, input(&user.id, "mine")).await.unwrap().unwrap();
        assert!(row.id.starts_with("cprov_"));
        assert_eq!(row.sort_order, 0);
        assert_eq!(count_custom_providers(&pool, &user.id).await.unwrap(), 1);
        assert_eq!(get_custom_provider_by_slug(&pool, &user.id, "mine").await.unwrap().unwrap(), row);
        assert!(get_custom_provider_by_slug(&pool, "someone-else", "mine").await.unwrap().is_none());
        assert!(get_custom_provider_by_id(&pool, &user.id, &row.id).await.unwrap().is_some());

        let second = insert_custom_provider(&pool, input(&user.id, "second")).await.unwrap().unwrap();
        assert_eq!(second.sort_order, 1);
        reorder_custom_providers(&pool, &user.id, &[second.id.clone(), row.id.clone()]).await.unwrap();
        let ids: Vec<_> = list_custom_providers(&pool, &user.id).await.unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![second.id.clone(), row.id.clone()]);

        update_custom_provider_fields(
            &pool,
            &row.id,
            CustomProviderPatch { name: Some("renamed"), count_tokens_url: Some(Some("https://x.test/count")), ..Default::default() },
        )
        .await
        .unwrap();
        let stored = get_custom_provider_by_id(&pool, &user.id, &row.id).await.unwrap().unwrap();
        assert_eq!(stored.name, "renamed");
        assert_eq!(stored.base_url, "https://example.test/v1", "an omitted field keeps its value");
        assert_eq!(stored.count_tokens_url.as_deref(), Some("https://x.test/count"));
        update_custom_provider_fields(&pool, &row.id, CustomProviderPatch { count_tokens_url: Some(None), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(get_custom_provider_by_id(&pool, &user.id, &row.id).await.unwrap().unwrap().count_tokens_url, None);

        assert!(!delete_custom_provider(&pool, "someone-else", &row.id).await.unwrap());
        assert!(delete_custom_provider(&pool, &user.id, &row.id).await.unwrap());
    }

    #[tokio::test]
    async fn a_cli_slug_blocks_the_custom_insert() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let user = insert_user(&pool, "slug@example.com").await;
        crate::db::cli::insert_cli_provider(
            &pool,
            crate::db::cli::NewCliProvider {
                user_id: &user.id,
                device_id: None,
                slug: "shared",
                name: "cli",
                format: "openai",
                models_json: None,
                model_filter_json: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert!(insert_custom_provider(&pool, input(&user.id, "shared")).await.unwrap().is_none());
        assert!(insert_custom_provider(&pool, input(&user.id, "free")).await.unwrap().is_some());
    }
}
