//! Builtin adapter lookup (apps/api/src/providers/index.ts `getAdapter`).

use std::sync::Arc;

use once_cell::sync::Lazy;

use super::types::DynAdapter;
use super::ProviderId;

static CLAUDE_CODE: Lazy<DynAdapter> = Lazy::new(super::claude_code::adapter);
static CODEX: Lazy<DynAdapter> = Lazy::new(super::codex::adapter);
static GROK: Lazy<DynAdapter> = Lazy::new(super::grok::adapter);
static ANTIGRAVITY: Lazy<DynAdapter> = Lazy::new(super::antigravity::adapter);

pub fn get_adapter(provider: ProviderId) -> DynAdapter {
    match provider {
        ProviderId::ClaudeCode => Arc::clone(&CLAUDE_CODE),
        ProviderId::Codex => Arc::clone(&CODEX),
        ProviderId::Grok => Arc::clone(&GROK),
        ProviderId::Antigravity => Arc::clone(&ANTIGRAVITY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_resolves_to_its_own_adapter() {
        for p in super::super::PROVIDERS {
            assert_eq!(get_adapter(p).id(), p.as_str());
        }
    }
}
