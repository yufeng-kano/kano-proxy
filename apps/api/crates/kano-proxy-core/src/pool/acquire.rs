//! Decrypted credential shape, the acquired account handed to adapters, and credential
//! persistence.
//!
//! Account and target selection live in `routing` (docs/providers.md § Routing module); what
//! remains here is turning a stored row into a usable credential and writing a refreshed one
//! back. Credentials are encrypted at rest with `TOKEN_ENCRYPTION_KEY` and never leave the
//! process in clear text.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::crypto::token_crypto::{decrypt_json, encrypt_json};
use crate::db::accounts::{update_account_payload, AccountIdentity, AccountRow};
use crate::AppState;

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

/// Encrypts `credential` and writes it to the account row, optionally refreshing the display
/// label and meta in the same statement (the COALESCE convention: `None` keeps the stored
/// value).
pub async fn save_credential(
    state: &AppState,
    account_id: &str,
    credential: &StoredCredential,
    meta: Option<AccountIdentity<'_>>,
) -> anyhow::Result<()> {
    let blob = encrypt_json(state.config().token_encryption_key.as_deref(), credential)?;
    update_account_payload(state.pool(), account_id, &blob, meta).await?;
    Ok(())
}

/// Decrypts a row's stored payload into the credential an adapter uses. A row whose payload
/// cannot be decrypted (a rotated `TOKEN_ENCRYPTION_KEY`, a truncated blob) is an error, never
/// a silently empty credential that would send an unauthenticated upstream request.
pub fn acquire(state: &AppState, row: AccountRow) -> anyhow::Result<AcquiredAccount> {
    let credential: StoredCredential =
        decrypt_json(state.config().token_encryption_key.as_deref(), &row.encrypted_payload)?;
    Ok(AcquiredAccount { row, credential })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_account, insert_user, skip_without_db, test_pool, test_state};
    use crate::upstream::MockTransport;

    fn credential() -> StoredCredential {
        StoredCredential {
            access_token: "at-1".into(),
            refresh_token: Some("rt-1".into()),
            expires_at: Some("2026-01-01T00:00:00.000Z".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_saved_credential_round_trips_through_the_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "acq@example.com").await;
        let row = insert_account(state.pool(), &user.id, "codex", &credential()).await;

        let acquired = acquire(&state, row.clone()).unwrap();
        assert_eq!(acquired.credential, credential());
        assert_eq!(acquired.row.id, row.id);
        // The stored payload is ciphertext, not the token.
        assert!(!row.encrypted_payload.contains("at-1"));

        let refreshed = StoredCredential { access_token: "at-2".into(), ..credential() };
        save_credential(&state, &row.id, &refreshed, Some(AccountIdentity { label: Some("renamed"), account_meta_json: None }))
            .await
            .unwrap();
        let stored = crate::db::accounts::get_account(state.pool(), &user.id, &row.id).await.unwrap().unwrap();
        assert_ne!(stored.encrypted_payload, row.encrypted_payload);
        assert_eq!(stored.label.as_deref(), Some("renamed"));
        assert_eq!(acquire(&state, stored).unwrap().credential.access_token, "at-2");
    }

    #[tokio::test]
    async fn an_undecryptable_payload_is_an_error() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, MockTransport::new());
        let user = insert_user(state.pool(), "bad@example.com").await;
        let mut row = insert_account(state.pool(), &user.id, "codex", &credential()).await;
        row.encrypted_payload = "not-a-ciphertext".into();
        assert!(acquire(&state, row).is_err());
    }
}
