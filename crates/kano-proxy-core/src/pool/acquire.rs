//! Decrypted credential shape and the acquired account handed to adapters
//! (apps/api/src/pool/acquire.ts). `save_credential` is added by the storage port.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::db::AccountRow;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredCredential {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone)]
pub struct AcquiredAccount {
    pub row: AccountRow,
    pub credential: StoredCredential,
}
