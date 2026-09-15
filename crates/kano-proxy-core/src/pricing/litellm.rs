//! Estimated per-request cost from LiteLLM's community price table plus the public OpenRouter
//! catalog for OpenRouter-specific ids (apps/api/src/pricing/litellm.ts, docs/pricing.md).
//!
//! The source tables are trimmed to the four per-token rates this proxy uses. The combined
//! table lives in [`crate::cache::Cache`] under `pricing:litellm:v1` with a 24h freshness
//! window behind a process-wide in-memory memo (the Worker's per-isolate memo), so the
//! request path costs no cache lookups in the common case and **never** a network fetch:
//! refreshes happen from the daily maintenance task, or inline from the admin usage-summary
//! route the first time after a deploy when the cache has nothing yet.
//!
//! Everything degrades to "cost unknown" (`None`): a fetch failure serves the last copy
//! regardless of age, an unmatched model prices as `None`. Never fabricate a rate, never fail
//! or delay a proxied request over pricing.

use std::sync::Mutex;
use std::time::Duration;

use indexmap::IndexMap;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::now_ms;
use crate::cache::Cache;
use crate::upstream::{UpstreamRequest, UpstreamTransport};
use crate::AppState;

pub const LITELLM_PRICING_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
pub const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// Cache key, unchanged from the KV namespace (docs/pricing.md).
pub const CACHE_KEY: &str = "pricing:litellm:v1";
/// Cache lifetime — long enough that a week of failed refreshes still stale-serves.
const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Age past which the daily maintenance task re-fetches.
pub const PRICING_FRESH_MS: i64 = 24 * 60 * 60 * 1000;
/// How long the process trusts its in-memory copy before re-consulting the cache.
const MEMO_RECHECK_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PriceSource {
    Litellm,
    Openrouter,
}

/// USD per token. `None` cache rates mean the table had none — see [`compute_cost`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    #[serde(rename = "cacheRead")]
    pub cache_read: Option<f64>,
    #[serde(rename = "cacheCreation")]
    pub cache_creation: Option<f64>,
    /// Source is persisted so OpenRouter rows cannot use a LiteLLM lookalike.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PriceSource>,
}

/// Keyed by lowercased LiteLLM model id; insertion order matches the JavaScript object.
pub type PriceTable = IndexMap<String, ModelPrice>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedTable {
    #[serde(rename = "fetchedAt")]
    fetched_at: i64,
    table: PriceTable,
    /// Present in snapshots created after source tagging was added.
    #[serde(rename = "litellmTable", default, skip_serializing_if = "Option::is_none")]
    litellm_table: Option<PriceTable>,
    #[serde(rename = "openRouterTable", default, skip_serializing_if = "Option::is_none")]
    open_router_table: Option<PriceTable>,
}

#[derive(Default)]
struct MemoState {
    memo: Option<CachedTable>,
    checked_at: i64,
}

static MEMO: Lazy<Mutex<MemoState>> = Lazy::new(|| Mutex::new(MemoState::default()));

fn memo_state() -> std::sync::MutexGuard<'static, MemoState> {
    MEMO.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clears the process-wide memo (the TypeScript `_resetPricingForTests`).
pub fn reset_pricing_for_tests() {
    let mut state = memo_state();
    state.memo = None;
    state.checked_at = 0;
}

/// Whether the loaded snapshot has separately persisted, source-tagged tables.
pub fn has_source_tagged_price_tables() -> bool {
    let state = memo_state();
    state.memo.as_ref().is_some_and(|m| m.litellm_table.is_some() && m.open_router_table.is_some())
}

/// Memo → cache → `None`. Never fetches. A cache failure or malformed entry falls back to
/// whatever the process already holds. The recheck window caches misses too, so a process
/// with no table (pre-first-refresh) does not pay a cache read on every log write.
pub async fn get_price_table(state: &AppState) -> Option<PriceTable> {
    get_price_table_with(state.cache()).await
}

/// Cache-only seam (the `AppState` wrapper above is what routes call).
pub async fn get_price_table_with(cache: &Cache) -> Option<PriceTable> {
    let now = now_ms();
    {
        let mut memo = memo_state();
        if now - memo.checked_at < MEMO_RECHECK_MS {
            return memo.memo.as_ref().map(|m| m.table.clone());
        }
        memo.checked_at = now;
    }
    if let Some(snap) = cache.get_json::<CachedTable>(CACHE_KEY).await {
        let mut memo = memo_state();
        memo.memo = Some(snap);
    }
    let memo = memo_state();
    memo.memo.as_ref().map(|m| m.table.clone())
}

async fn fetch_and_trim(
    transport: &dyn UpstreamTransport,
    url: &str,
    trim: fn(&Value) -> PriceTable,
) -> Option<PriceTable> {
    let request = UpstreamRequest::get(url).header("accept", "application/json");
    let response = transport.send(request).await.ok()?;
    if !response.status.is_success() {
        return None;
    }
    let json = response.json_value().await.ok()?;
    let table = trim(&json);
    // An empty trim means the upstream shape changed — do not replace a working source table
    // with nothing.
    if table.is_empty() {
        None
    } else {
        Some(table)
    }
}

/// Fetch + trim + store both sources. A failure of either source stale-serves only that
/// source's prior copy, so an OpenRouter outage cannot discard LiteLLM prices (or vice
/// versa). Never fails.
pub async fn refresh_price_table(state: &AppState) -> Option<PriceTable> {
    refresh_price_table_with(state.transport().as_ref(), state.cache()).await
}

pub async fn refresh_price_table_with(
    transport: &dyn UpstreamTransport,
    cache: &Cache,
) -> Option<PriceTable> {
    let (fresh_litellm, fresh_open_router) = futures::future::join(
        fetch_and_trim(transport, LITELLM_PRICING_URL, trim_litellm_table),
        fetch_and_trim(transport, OPENROUTER_MODELS_URL, trim_openrouter_table),
    )
    .await;

    let (litellm_table, open_router_table, previous_table) = {
        let memo = memo_state();
        // Legacy combined snapshots have no provenance: preserve their LiteLLM entries for
        // non-OpenRouter traffic only, and never treat any of their openrouter/<id> entries
        // as catalog prices. A source-tagged snapshot's `table` is just a merged read view
        // and must not become a legacy fallback.
        let legacy_litellm = match memo.memo.as_ref() {
            Some(m) if m.litellm_table.is_none() && m.open_router_table.is_none() => {
                strip_openrouter_entries(&m.table)
            }
            _ => PriceTable::new(),
        };
        // Empty source tables record an attempted fetch, letting the cache retain its normal
        // 24h refresh cadence after a source outage.
        let litellm_table = fresh_litellm
            .or_else(|| memo.memo.as_ref().and_then(|m| m.litellm_table.clone()))
            .unwrap_or(legacy_litellm);
        let open_router_table = fresh_open_router
            .or_else(|| memo.memo.as_ref().and_then(|m| m.open_router_table.clone()))
            .unwrap_or_default();
        (litellm_table, open_router_table, memo.memo.as_ref().map(|m| m.table.clone()))
    };

    // The public OpenRouter catalog is authoritative for its exact model keys.
    let mut table = litellm_table.clone();
    for (key, value) in &open_router_table {
        table.insert(key.clone(), value.clone());
    }
    if table.is_empty() {
        return previous_table;
    }

    let snap = CachedTable {
        fetched_at: now_ms(),
        table: table.clone(),
        litellm_table: Some(litellm_table),
        open_router_table: Some(open_router_table),
    };
    {
        let mut memo = memo_state();
        memo.checked_at = snap.fetched_at;
        memo.memo = Some(snap.clone());
    }
    cache.put_json(CACHE_KEY, &snap, CACHE_TTL).await;
    Some(table)
}

/// Daily maintenance entry: refetch when a source is missing, or the stored table is stale.
pub async fn ensure_fresh_price_table(state: &AppState) {
    ensure_fresh_price_table_with(state.transport().as_ref(), state.cache()).await;
}

pub async fn ensure_fresh_price_table_with(transport: &dyn UpstreamTransport, cache: &Cache) {
    get_price_table_with(cache).await;
    {
        let memo = memo_state();
        let fresh = memo.memo.as_ref().is_some_and(|m| {
            m.litellm_table.is_some()
                && m.open_router_table.is_some()
                && now_ms() - m.fetched_at < PRICING_FRESH_MS
        });
        if fresh {
            return;
        }
    }
    refresh_price_table_with(transport, cache).await;
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|n| n.is_finite())
}

/// Keeps only entries that carry a usable rate; keys lowercased for lookup.
pub fn trim_litellm_table(json: &Value) -> PriceTable {
    let mut out = PriceTable::new();
    let Some(obj) = json.as_object() else {
        return out;
    };
    for (key, value) in obj {
        // "sample_spec" is LiteLLM's schema-documentation entry, not a model.
        if key == "sample_spec" || !value.is_object() {
            continue;
        }
        let input = num(value.get("input_cost_per_token"));
        let output = num(value.get("output_cost_per_token"));
        if input.is_none() && output.is_none() {
            continue;
        }
        out.insert(
            key.to_lowercase(),
            ModelPrice {
                input: input.unwrap_or(0.0),
                output: output.unwrap_or(0.0),
                cache_read: num(value.get("cache_read_input_token_cost")),
                cache_creation: num(value.get("cache_creation_input_token_cost")),
                source: None,
            },
        );
    }
    out
}

fn decimal(v: Option<&Value>) -> Option<f64> {
    let s = v.and_then(Value::as_str)?;
    if s.trim().is_empty() {
        return None;
    }
    s.trim().parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Legacy combined snapshots cannot prove an OpenRouter price's source.
fn strip_openrouter_entries(table: &PriceTable) -> PriceTable {
    table
        .iter()
        .filter(|(key, _)| !key.to_lowercase().starts_with("openrouter/"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Add OpenRouter's default rates only under its complete catalog id. Conditional overrides
/// cannot be applied from the aggregate usage we log, so skip them rather than guessing.
/// Never use these entries for another provider.
pub fn trim_openrouter_table(json: &Value) -> PriceTable {
    let mut out = PriceTable::new();
    let Some(data) = json.get("data").and_then(Value::as_array) else {
        return out;
    };
    for model in data {
        if !model.is_object() {
            continue;
        }
        let Some(id) = model.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(pricing) = model.get("pricing").filter(|p| p.is_object()) else {
            continue;
        };
        if pricing.get("overrides").and_then(Value::as_array).is_some_and(|o| !o.is_empty()) {
            continue;
        }
        let (Some(input), Some(output)) = (decimal(pricing.get("prompt")), decimal(pricing.get("completion")))
        else {
            // We cannot safely price a partially specified token pair as a zero rate.
            continue;
        };
        out.insert(
            format!("openrouter/{}", normalize_id(id)),
            ModelPrice {
                input,
                output,
                cache_read: decimal(pricing.get("input_cache_read")),
                cache_creation: decimal(pricing.get("input_cache_write")),
                source: Some(PriceSource::Openrouter),
            },
        );
    }
    out
}

static BRACKET_VARIANT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[[^\]]*\]$").expect("bracket variant regex"));
static EFFORT_SUFFIX_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"-(?:thinking-)?(?:high|medium|low|tiered)$").expect("effort suffix regex")
});
static THINKING_SUFFIX_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"-(?:thinking|thought)$").expect("thinking suffix regex"));

/// Lowercase, trimmed, bracket variant stripped: "claude-opus-5[1m]" → "claude-opus-5".
fn normalize_id(id: &str) -> String {
    let lowered = id.trim().to_lowercase();
    BRACKET_VARIANT_RE.replace(&lowered, "").into_owned()
}

const PREFIXES: [&str; 6] = ["anthropic", "openai", "xai", "gemini", "vertex_ai", "openrouter"];

fn usable(table: &PriceTable, key: &str) -> Option<ModelPrice> {
    let hit = table.get(key)?;
    if hit.source == Some(PriceSource::Openrouter) {
        return None;
    }
    Some(hit.clone())
}

/// `raw_model` is this proxy's `provider/upstream…` id; table keys are LiteLLM's own,
/// sometimes vendor-prefixed. Matching chain (docs/pricing.md): the upstream id exact, then
/// with its own leading path segments progressively stripped, then common LiteLLM prefix
/// forms of each candidate. First hit wins; no hit is `None`, never a guess.
pub fn resolve_model_price(table: &PriceTable, raw_model: &str) -> Option<ModelPrice> {
    let slash = raw_model.find('/');
    let provider = normalize_id(match slash {
        None => "",
        Some(i) => &raw_model[..i],
    });
    let upstream = normalize_id(match slash {
        None => raw_model,
        Some(i) => &raw_model[i + 1..],
    });
    if upstream.is_empty() {
        return None;
    }

    // OpenRouter's catalog prices an exact provider/model id. Do not let an unrelated bare,
    // LiteLLM, or Cloudflare entry price an OpenRouter request.
    if provider == "openrouter" {
        let hit = table.get(&format!("openrouter/{upstream}"))?;
        return if hit.source == Some(PriceSource::Openrouter) { Some(hit.clone()) } else { None };
    }

    let mut base_candidates: Vec<String> = vec![upstream.clone()];
    let mut rest = upstream.clone();
    while let Some(i) = rest.find('/') {
        rest = rest[i + 1..].to_string();
        if !rest.is_empty() {
            base_candidates.push(rest.clone());
        } else {
            break;
        }
    }

    // 1. Bare exact candidate matches across all path segments first
    for c in &base_candidates {
        if let Some(hit) = usable(table, c) {
            return Some(hit);
        }
    }

    // 2. Vendor-prefixed exact matches across all path segments
    for c in &base_candidates {
        for p in PREFIXES {
            if let Some(hit) = usable(table, &format!("{p}/{c}")) {
                return Some(hit);
            }
        }
    }

    // 3. Antigravity / Gemini reasoning effort & preview fallback. Restricted to verified
    // Antigravity or Gemini model id segments to prevent guessing rates for arbitrary
    // non-Gemini models (e.g. notagemini-high).
    let is_gemini_model = |c: &str| {
        provider == "antigravity"
            || c.starts_with("gemini-")
            || c.starts_with("gemini/")
            || c == "gemini"
            || c.contains("/gemini-")
            || c.ends_with("/gemini")
    };

    let mut variant_candidates: Vec<String> = Vec::new();
    for c in &base_candidates {
        if !is_gemini_model(c) {
            continue;
        }
        let stripped = {
            let once = EFFORT_SUFFIX_RE.replace(c, "").into_owned();
            THINKING_SUFFIX_RE.replace(&once, "").into_owned()
        };
        if &stripped != c {
            variant_candidates.push(stripped.clone());
        }
        if !c.ends_with("-preview") {
            variant_candidates.push(format!("{c}-preview"));
            if &stripped != c {
                variant_candidates.push(format!("{stripped}-preview"));
            }
        } else {
            variant_candidates.push(c[..c.len() - "-preview".len()].to_string());
        }
    }

    if !variant_candidates.is_empty() {
        // 3a. Bare variant matches
        for v in &variant_candidates {
            if let Some(hit) = usable(table, v) {
                return Some(hit);
            }
        }
        // 3b. Prefixed variant matches
        for v in &variant_candidates {
            for p in PREFIXES {
                if let Some(hit) = usable(table, &format!("{p}/{v}")) {
                    return Some(hit);
                }
            }
        }
    }

    None
}

/// Token counts as stored on a request log row; all four may be absent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CostUsage {
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

/// `prompt_tokens` is the stored **total** input count including cache reads and writes
/// (docs/database.md), so the uncached component is the remainder, floored at 0 against
/// inconsistent upstream numbers. A table entry without cache rates bills cached input at the
/// plain input rate. All-absent usage is `None` — nothing reported prices as nothing known,
/// not $0.
pub fn compute_cost(price: &ModelPrice, usage: &CostUsage) -> Option<f64> {
    if usage.prompt_tokens.is_none()
        && usage.completion_tokens.is_none()
        && usage.cache_read_input_tokens.is_none()
        && usage.cache_creation_input_tokens.is_none()
    {
        return None;
    }
    let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
    let cache_creation = usage.cache_creation_input_tokens.unwrap_or(0);
    let uncached = (usage.prompt_tokens.unwrap_or(0) - cache_read - cache_creation).max(0);
    Some(
        uncached as f64 * price.input
            + cache_read as f64 * price.cache_read.unwrap_or(price.input)
            + cache_creation as f64 * price.cache_creation.unwrap_or(price.input)
            + usage.completion_tokens.unwrap_or(0) as f64 * price.output,
    )
}

/// resolve + compute in one step — `None` when the model has no price or usage is all-absent.
pub fn estimate_cost(table: &PriceTable, raw_model: &str, usage: &CostUsage) -> Option<f64> {
    let price = resolve_model_price(table, raw_model)?;
    compute_cost(&price, usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::transport::TransportError;
    use crate::upstream::{MockTransport, UpstreamResponse};
    use http::StatusCode;
    use serde_json::json;
    use std::sync::Arc;

    /// The memo is process-wide, so the lifecycle tests take turns on it.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn guard() -> tokio::sync::MutexGuard<'static, ()> {
        let g = SERIAL.lock().await;
        reset_pricing_for_tests();
        g
    }

    fn openrouter_json() -> Value {
        json!({
            "data": [
                {
                    "id": "z-ai/glm-5.2",
                    "pricing": { "prompt": "0.00000076", "completion": "0.00000242", "input_cache_read": "0.00000014" },
                },
                { "id": "conditional/model", "pricing": { "prompt": "0.000001", "completion": "0.000002", "overrides": [{}] } },
                { "id": "prompt-only/model", "pricing": { "prompt": "0.000001" } },
                { "id": "completion-only/model", "pricing": { "completion": "0.000002" } },
            ]
        })
    }

    fn litellm_json() -> Value {
        json!({
            "sample_spec": { "input_cost_per_token": 0, "output_cost_per_token": 0 },
            "claude-opus-5": {
                "input_cost_per_token": 0.000015,
                "output_cost_per_token": 0.000075,
                "cache_read_input_token_cost": 0.0000015,
                "cache_creation_input_token_cost": 0.00001875,
                "litellm_provider": "anthropic",
                "mode": "chat",
            },
            "gpt-4o-mini": { "input_cost_per_token": 0.00000015, "output_cost_per_token": 0.0000006, "litellm_provider": "openai" },
            "xai/grok-4.5": { "input_cost_per_token": 0.000003, "output_cost_per_token": 0.000015 },
            "openrouter/openai/gpt-5.6-luna": { "input_cost_per_token": 0.00000125, "output_cost_per_token": 0.00001 },
            "gemini-3.7-flash": {
                "input_cost_per_token": 0.00000075,
                "output_cost_per_token": 0.00000375,
                "cache_read_input_token_cost": 0.000000075,
            },
            "gemini-3-flash-preview": { "input_cost_per_token": 0.0000005, "output_cost_per_token": 0.0000025 },
            "no-rates-model": { "litellm_provider": "openai", "mode": "chat" },
        })
    }

    fn price(input: f64, output: f64, cache_read: Option<f64>, cache_creation: Option<f64>) -> ModelPrice {
        ModelPrice { input, output, cache_read, cache_creation, source: None }
    }

    fn table_of(entries: &[(&str, ModelPrice)]) -> PriceTable {
        entries.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
    }

    /// Answers both pricing URLs from one handler, so the concurrent fetches do not depend on
    /// handler order. `litellm`/`open_router` of `None` answer HTTP 500 for that source.
    fn expect_pricing(mock: &MockTransport, litellm: Option<Value>, open_router: Option<Value>) {
        for _ in 0..2 {
            let litellm = litellm.clone();
            let open_router = open_router.clone();
            mock.expect(move |req| {
                let body = if req.url.contains("openrouter.ai") { &open_router } else { &litellm };
                Ok(match body {
                    Some(v) => UpstreamResponse::json(StatusCode::OK, v),
                    None => UpstreamResponse::from_bytes(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        http::HeaderMap::new(),
                        "down",
                    ),
                })
            });
        }
    }

    fn unreachable(mock: &MockTransport) {
        for _ in 0..2 {
            mock.expect(|_| Err(TransportError::Connect("network down".into())));
        }
    }

    #[test]
    fn trim_litellm_keeps_only_priced_entries() {
        let table = trim_litellm_table(&litellm_json());
        assert!(table.get("sample_spec").is_none());
        assert!(table.get("no-rates-model").is_none());
        assert_eq!(
            table.get("claude-opus-5"),
            Some(&price(0.000015, 0.000075, Some(0.0000015), Some(0.00001875)))
        );
        let mini = table.get("gpt-4o-mini").expect("gpt-4o-mini priced");
        assert_eq!((mini.cache_read, mini.cache_creation), (None, None));
    }

    #[test]
    fn trim_litellm_ignores_malformed_entries() {
        let table = trim_litellm_table(&json!({
            "good": { "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6 },
            "bad1": "string",
            "bad2": null,
            "bad3": { "input_cost_per_token": "not a number" },
        }));
        assert_eq!(table.keys().cloned().collect::<Vec<_>>(), vec!["good".to_string()]);
    }

    #[test]
    fn trim_openrouter_maps_only_complete_default_rates() {
        let table = trim_openrouter_table(&openrouter_json());
        assert_eq!(
            table,
            table_of(&[(
                "openrouter/z-ai/glm-5.2",
                ModelPrice {
                    input: 0.00000076,
                    output: 0.00000242,
                    cache_read: Some(0.00000014),
                    cache_creation: None,
                    source: Some(PriceSource::Openrouter),
                },
            )])
        );
    }

    #[test]
    fn trim_openrouter_skips_conditional_partial_and_malformed_prices() {
        assert!(trim_openrouter_table(&json!({ "data": [{ "id": "bad", "pricing": { "prompt": "nope" } }] })).is_empty());
        let rest = json!({ "data": openrouter_json()["data"].as_array().unwrap()[1..].to_vec() });
        assert!(trim_openrouter_table(&rest).is_empty());
        assert!(trim_openrouter_table(&json!({ "data": [] })).is_empty());
        assert!(trim_openrouter_table(&json!("nope")).is_empty());
    }

    #[test]
    fn matches_the_bare_upstream_id_after_stripping_the_provider_prefix() {
        let table = trim_litellm_table(&litellm_json());
        assert!(resolve_model_price(&table, "claude-code/claude-opus-5").is_some());
        assert!(resolve_model_price(&table, "my-endpoint/gpt-4o-mini").is_some());
    }

    #[test]
    fn strips_a_bracket_variant_suffix_before_matching() {
        let table = trim_litellm_table(&litellm_json());
        assert!(resolve_model_price(&table, "claude-code/claude-opus-5[1m]").is_some());
        assert!(resolve_model_price(&table, "claude-code/Claude-Opus-5[1M]").is_some());
    }

    #[test]
    fn tries_vendor_prefixed_litellm_keys() {
        let table = trim_litellm_table(&litellm_json());
        assert_eq!(resolve_model_price(&table, "grok/grok-4.5"), Some(price(0.000003, 0.000015, None, None)));
    }

    #[test]
    fn openrouter_ids_need_an_exact_catalog_key() {
        let table = trim_litellm_table(&litellm_json());
        let catalog = trim_openrouter_table(&openrouter_json());
        assert_eq!(
            resolve_model_price(&catalog, "openrouter/z-ai/glm-5.2"),
            Some(ModelPrice {
                input: 0.00000076,
                output: 0.00000242,
                cache_read: Some(0.00000014),
                cache_creation: None,
                source: Some(PriceSource::Openrouter),
            })
        );
        assert_eq!(resolve_model_price(&table, "openrouter/openai/gpt-5.6-luna"), None);
        let bare = table_of(&[("z-ai/glm-5.2", price(1.0, 1.0, None, None))]);
        assert_eq!(resolve_model_price(&bare, "openrouter/z-ai/glm-5.2"), None);
    }

    #[test]
    fn does_not_use_a_litellm_openrouter_lookalike() {
        let lookalike = table_of(&[("openrouter/z-ai/glm-5.2", price(1.0, 1.0, None, None))]);
        assert_eq!(resolve_model_price(&lookalike, "openrouter/z-ai/glm-5.2"), None);
    }

    #[test]
    fn progressively_strips_the_upstream_ids_own_path_segments() {
        let table = trim_litellm_table(&litellm_json());
        assert!(resolve_model_price(&table, "byok/openai/gpt-4o-mini").is_some());
    }

    #[test]
    fn resolves_effort_tiered_and_thinking_variants_to_the_base_rate() {
        let table = trim_litellm_table(&litellm_json());
        let expected = Some(price(0.00000075, 0.00000375, Some(0.000000075), None));
        for raw in [
            "antigravity/gemini-3.7-flash-high",
            "antigravity/gemini-3.7-flash-medium",
            "antigravity/gemini-3.7-flash-low",
            "antigravity/gemini-3.7-flash-thinking",
            "antigravity/gemini-3.7-flash-tiered",
            "antigravity/gemini-3.7-flash-high[1M]",
        ] {
            assert_eq!(resolve_model_price(&table, raw), expected, "{raw}");
        }
    }

    #[test]
    fn resolves_preview_suffix_variants() {
        let table = trim_litellm_table(&litellm_json());
        assert_eq!(
            resolve_model_price(&table, "antigravity/gemini-3-flash"),
            Some(price(0.0000005, 0.0000025, None, None))
        );
    }

    #[test]
    fn restricts_the_effort_suffix_fallback_to_gemini_models() {
        let table = trim_litellm_table(&litellm_json());
        assert_eq!(resolve_model_price(&table, "custom/gpt-4o-mini-high"), None);
        let custom = table_of(&[("notagemini", price(1.0, 1.0, None, None))]);
        assert_eq!(resolve_model_price(&custom, "custom/notagemini-high"), None);
    }

    #[test]
    fn preserves_bare_candidate_precedence_over_vendor_prefixed_matches() {
        let custom = table_of(&[
            ("gpt-4o-mini", price(1.0, 1.0, None, None)),
            ("openai/sub/gpt-4o-mini", price(99.0, 99.0, None, None)),
        ]);
        assert_eq!(resolve_model_price(&custom, "custom/sub/gpt-4o-mini"), Some(price(1.0, 1.0, None, None)));
    }

    #[test]
    fn returns_none_on_no_match() {
        let table = trim_litellm_table(&litellm_json());
        assert_eq!(resolve_model_price(&table, "claude-code/some-unknown-model"), None);
        assert_eq!(resolve_model_price(&table, ""), None);
    }

    fn sample_price() -> ModelPrice {
        price(0.00001, 0.00005, Some(0.000001), Some(0.0000125))
    }

    fn close(a: Option<f64>, b: f64) {
        let got = a.expect("a cost");
        assert!((got - b).abs() < 1e-12, "{got} != {b}");
    }

    #[test]
    fn splits_prompt_tokens_into_uncached_and_cached_components() {
        let cost = compute_cost(
            &sample_price(),
            &CostUsage {
                prompt_tokens: Some(1000),
                completion_tokens: Some(400),
                cache_read_input_tokens: Some(200),
                cache_creation_input_tokens: Some(100),
            },
        );
        close(cost, 700.0 * 0.00001 + 200.0 * 0.000001 + 100.0 * 0.0000125 + 400.0 * 0.00005);
    }

    #[test]
    fn bills_cached_input_at_the_plain_rate_without_cache_rates() {
        let flat = price(0.00001, 0.00005, None, None);
        let cost = compute_cost(
            &flat,
            &CostUsage {
                prompt_tokens: Some(1000),
                completion_tokens: Some(0),
                cache_read_input_tokens: Some(400),
                cache_creation_input_tokens: Some(0),
            },
        );
        close(cost, 1000.0 * 0.00001);
    }

    #[test]
    fn floors_the_uncached_remainder_at_zero() {
        let cost = compute_cost(
            &sample_price(),
            &CostUsage {
                prompt_tokens: Some(100),
                completion_tokens: Some(0),
                cache_read_input_tokens: Some(150),
                cache_creation_input_tokens: Some(0),
            },
        );
        close(cost, 150.0 * 0.000001);
    }

    #[test]
    fn partially_absent_usage_is_zeros_but_all_absent_is_unknown() {
        close(
            compute_cost(&sample_price(), &CostUsage { prompt_tokens: Some(100), ..Default::default() }),
            100.0 * 0.00001,
        );
        assert_eq!(compute_cost(&sample_price(), &CostUsage::default()), None);
    }

    #[test]
    fn estimate_cost_is_none_for_an_unpriced_model() {
        let table = PriceTable::new();
        assert_eq!(
            estimate_cost(
                &table,
                "claude-code/whatever",
                &CostUsage {
                    prompt_tokens: Some(100),
                    completion_tokens: Some(10),
                    cache_read_input_tokens: Some(0),
                    cache_creation_input_tokens: Some(0),
                },
            ),
            None
        );
    }

    #[tokio::test]
    async fn get_price_table_is_none_before_anything_was_fetched_and_never_fetches() {
        let _g = guard().await;
        let cache = Cache::new();
        let mock = MockTransport::new();
        assert!(get_price_table_with(&cache).await.is_none());
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    async fn refresh_stores_the_trimmed_table_and_get_serves_it_from_the_cache() {
        let _g = guard().await;
        let cache = Cache::new();
        let mock = MockTransport::new();
        expect_pricing(&mock, Some(litellm_json()), Some(openrouter_json()));
        let table = refresh_price_table_with(mock.as_ref(), &cache).await.expect("a table");
        assert!(table.contains_key("claude-opus-5"));

        reset_pricing_for_tests(); // force the cache path
        let from_cache = get_price_table_with(&cache).await.expect("a cached table");
        assert!(from_cache.contains_key("claude-opus-5"));
    }

    #[tokio::test]
    async fn a_failed_refresh_keeps_the_previous_table() {
        let _g = guard().await;
        let cache = Cache::new();
        let mock = MockTransport::new();
        expect_pricing(&mock, Some(litellm_json()), Some(openrouter_json()));
        refresh_price_table_with(mock.as_ref(), &cache).await;
        expect_pricing(&mock, None, None);
        let table = refresh_price_table_with(mock.as_ref(), &cache).await.expect("stale-served");
        assert!(table.contains_key("claude-opus-5"));
    }

    #[tokio::test]
    async fn a_refresh_that_trims_to_nothing_keeps_the_previous_table() {
        let _g = guard().await;
        let cache = Cache::new();
        let mock = MockTransport::new();
        expect_pricing(&mock, Some(litellm_json()), Some(openrouter_json()));
        refresh_price_table_with(mock.as_ref(), &cache).await;
        expect_pricing(&mock, Some(json!({ "sample_spec": {} })), Some(json!({ "data": [] })));
        let table = refresh_price_table_with(mock.as_ref(), &cache).await.expect("stale-served");
        assert!(table.contains_key("claude-opus-5"));
    }

    #[tokio::test]
    async fn a_network_error_resolves_to_none_when_nothing_was_ever_cached() {
        let _g = guard().await;
        let cache = Cache::new();
        let mock = MockTransport::new();
        unreachable(&mock);
        assert!(refresh_price_table_with(mock.as_ref(), &cache).await.is_none());
    }

    #[tokio::test]
    async fn ensure_fresh_skips_both_source_fetches_while_the_tables_are_fresh() {
        let _g = guard().await;
        let cache = Cache::new();
        let mock = MockTransport::new();
        expect_pricing(&mock, Some(litellm_json()), Some(openrouter_json()));
        ensure_fresh_price_table_with(mock.as_ref(), &cache).await;
        ensure_fresh_price_table_with(mock.as_ref(), &cache).await;
        assert_eq!(mock.requests().len(), 2);
    }

    #[tokio::test]
    async fn stale_serves_one_source_when_the_other_refresh_fails() {
        let _g = guard().await;
        let cache = Cache::new();
        let mock = MockTransport::new();
        expect_pricing(&mock, Some(litellm_json()), Some(openrouter_json()));
        refresh_price_table_with(mock.as_ref(), &cache).await;
        expect_pricing(&mock, Some(litellm_json()), None);
        let table = refresh_price_table_with(mock.as_ref(), &cache).await.expect("a table");
        assert_eq!(
            resolve_model_price(&table, "openrouter/z-ai/glm-5.2"),
            Some(ModelPrice {
                input: 0.00000076,
                output: 0.00000242,
                cache_read: Some(0.00000014),
                cache_creation: None,
                source: Some(PriceSource::Openrouter),
            })
        );
    }

    async fn seed_legacy_snapshot(cache: &Cache) {
        let snapshot = json!({
            "fetchedAt": now_ms(),
            "table": {
                "claude-opus-5": { "input": 1, "output": 1, "cacheRead": null, "cacheCreation": null },
                "openrouter/z-ai/glm-5.2": { "input": 1, "output": 1, "cacheRead": null, "cacheCreation": null },
            },
        });
        cache
            .put(CACHE_KEY, bytes::Bytes::from(serde_json::to_vec(&snapshot).unwrap()), CACHE_TTL)
            .await;
    }

    #[tokio::test]
    async fn never_authorizes_a_legacy_snapshots_openrouter_entry() {
        let _g = guard().await;
        let cache = Cache::new();
        seed_legacy_snapshot(&cache).await;
        let legacy = get_price_table_with(&cache).await.expect("legacy snapshot");
        assert_eq!(resolve_model_price(&legacy, "openrouter/z-ai/glm-5.2"), None);

        let mock = MockTransport::new();
        expect_pricing(&mock, Some(litellm_json()), None);
        let refreshed = refresh_price_table_with(mock.as_ref(), &cache).await.expect("a table");
        assert_eq!(resolve_model_price(&refreshed, "openrouter/z-ai/glm-5.2"), None);
        assert!(resolve_model_price(&refreshed, "claude-code/claude-opus-5").is_some());
    }

    #[tokio::test]
    async fn refreshes_a_fresh_legacy_snapshot_and_prices_openrouter_after_its_catalog_succeeds() {
        let _g = guard().await;
        let cache = Cache::new();
        seed_legacy_snapshot(&cache).await;
        let legacy = get_price_table_with(&cache).await.expect("legacy snapshot");
        assert!(resolve_model_price(&legacy, "claude-code/claude-opus-5").is_some());
        assert_eq!(resolve_model_price(&legacy, "openrouter/z-ai/glm-5.2"), None);

        let mock = MockTransport::new();
        expect_pricing(&mock, Some(litellm_json()), Some(openrouter_json()));
        ensure_fresh_price_table_with(mock.as_ref(), &cache).await;
        assert_eq!(mock.requests().len(), 2);

        reset_pricing_for_tests();
        let refreshed = get_price_table_with(&cache).await.expect("a refreshed table");
        assert_eq!(
            resolve_model_price(&refreshed, "openrouter/z-ai/glm-5.2"),
            Some(ModelPrice {
                input: 0.00000076,
                output: 0.00000242,
                cache_read: Some(0.00000014),
                cache_creation: None,
                source: Some(PriceSource::Openrouter),
            })
        );
    }

    #[tokio::test]
    async fn keeps_litellm_prices_when_openrouter_is_unavailable_on_the_first_refresh() {
        let _g = guard().await;
        let cache = Cache::new();
        let mock = MockTransport::new();
        expect_pricing(&mock, Some(litellm_json()), None);
        let table = refresh_price_table_with(mock.as_ref(), &cache).await.expect("a table");
        assert!(table.contains_key("claude-opus-5"));
    }

    #[tokio::test]
    async fn the_app_state_wrappers_reach_the_same_seams() {
        let _g = guard().await;
        let mock: Arc<MockTransport> = MockTransport::new();
        expect_pricing(&mock, Some(litellm_json()), Some(openrouter_json()));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused@127.0.0.1/unused")
            .expect("a lazy pool never dials");
        let state = AppState::builder(crate::db::test_support::test_config(), pool)
            .transport(mock.clone())
            .build();
        let table = refresh_price_table(&state).await.expect("a table");
        assert!(table.contains_key("claude-opus-5"));
        assert!(get_price_table(&state).await.is_some());
        assert!(has_source_tagged_price_tables());
    }
}
