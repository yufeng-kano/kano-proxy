//! `upstream_accounts` rows (apps/api/src/db/accounts.ts). Queries are added by the storage
//! port; the row shape is shared with routing, pool and providers.

use serde::{Deserialize, Serialize};

/// `provider` is a builtin `ProviderId` or a custom/CLI provider slug.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct AccountRow {
    pub id: String,
    pub user_id: String,
    pub provider: String,
    pub external_account_id: Option<String>,
    pub label: Option<String>,
    pub custom_label: Option<String>,
    pub priority: i32,
    pub encrypted_payload: String,
    pub account_meta_json: Option<String>,
    pub usage_snapshot_json: Option<String>,
    pub usage_fetched_at: Option<String>,
    pub usage_fetching_at: Option<String>,
    pub bench_until: Option<String>,
    pub bench_reason: Option<String>,
    pub refreshing_at: Option<String>,
    pub edge_strikes: i32,
    pub edge_strike_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
