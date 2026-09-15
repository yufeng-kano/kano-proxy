//! `custom_providers` rows (apps/api/src/db/custom_providers.ts). Queries are added by the
//! storage port; the row shape is shared with routing and the custom adapters.

use serde::{Deserialize, Serialize};

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
