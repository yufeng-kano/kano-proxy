//! `/api/usage` — the Overview dashboard's aggregate (apps/api/src/routes/usage.ts,
//! docs/admin-ui.md § Overview page, docs/pricing.md).
//!
//! `GET /summary` takes either an explicit `?from&to&grain&offset` range (what the range
//! picker sends) or the legacy trailing `?days=` window. The SQL is a plain scoped read; all
//! aggregation is the pure math below, so it is unit-testable without a database.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use indexmap::IndexMap;
use serde_json::{json, Value};

use crate::auth::session::SessionUser;
use crate::db::accounts::{iso_from_ms, parse_iso_ms};
use crate::db::cli::list_cli_providers;
use crate::db::custom_providers::list_custom_providers;
use crate::db::request_logs::{usage_rows_in_range, RequestLogPageRow, UsageLogRow};
use crate::pricing::litellm::{
    estimate_cost, get_price_table, has_source_tagged_price_tables, refresh_price_table, CostUsage, PriceTable,
};
use crate::providers::{ProviderId, PROVIDERS};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/summary", get(summary))
}

const ALLOWED_DAYS: [i64; 3] = [1, 7, 30];
const DAY_MS: i64 = 86_400_000;
/// A range wider than a year is refused rather than scanned.
const MAX_SPAN_MS: i64 = 366 * DAY_MS;

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// `hour` buckets by `YYYY-MM-DDTHH`, `day` by `YYYY-MM-DD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grain {
    Hour,
    Day,
}

impl Grain {
    pub fn as_str(self) -> &'static str {
        match self {
            Grain::Hour => "hour",
            Grain::Day => "day",
        }
    }
}

/// Totals over a row set (apps/api/src/routes/usage.ts `Totals`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totals {
    pub requests: i64,
    pub errors: i64,
    pub avg_latency_ms: Option<i64>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_rate: Option<f64>,
    pub usage_known_requests: i64,
    pub cache_known_requests: i64,
    /// Estimated USD over priced rows; `None` when none is priced (docs/pricing.md).
    pub cost: Option<f64>,
    /// Rows contributing to `cost` — lets the UI annotate partial coverage.
    pub cost_known_requests: i64,
}

/// Shared totals math — used for the overall summary and for each (provider, model) group.
pub fn accumulate(rows: &[UsageLogRow]) -> Totals {
    let mut totals = Totals { requests: rows.len() as i64, ..Totals::default() };
    let mut latency_sum: i64 = 0;
    let mut cache_rate_numerator: i64 = 0;
    let mut cache_rate_denominator: i64 = 0;
    let mut cache_rate_rows: i64 = 0;
    for row in rows {
        if row.status_code >= 400 {
            totals.errors += 1;
        }
        latency_sum += row.latency_ms;
        if let Some(prompt) = row.prompt_tokens {
            totals.prompt_tokens += prompt;
            totals.usage_known_requests += 1;
        }
        if let Some(completion) = row.completion_tokens {
            totals.completion_tokens += completion;
        }
        if let Some(cache_read) = row.cache_read_input_tokens {
            totals.cache_read_input_tokens += cache_read;
            totals.cache_known_requests += 1;
            if let Some(prompt) = row.prompt_tokens.filter(|p| *p > 0) {
                cache_rate_numerator += cache_read;
                cache_rate_denominator += prompt;
                cache_rate_rows += 1;
            }
        }
        if let Some(cache_creation) = row.cache_creation_input_tokens {
            totals.cache_creation_input_tokens += cache_creation;
        }
        if let Some(cost) = row.cost {
            totals.cost = Some(totals.cost.unwrap_or(0.0) + cost);
            totals.cost_known_requests += 1;
        }
    }
    totals.avg_latency_ms = if rows.is_empty() {
        None
    } else {
        Some(js_round(latency_sum as f64 / rows.len() as f64))
    };
    totals.cache_rate = if cache_rate_rows > 0 {
        Some(cache_rate_numerator as f64 / cache_rate_denominator as f64)
    } else {
        None
    };
    totals
}

/// `Math.round` — half up, which for these non-negative averages is `f64::round`.
fn js_round(value: f64) -> i64 {
    value.round() as i64
}

fn totals_json(totals: &Totals) -> Value {
    json!({
        "requests": totals.requests,
        "errors": totals.errors,
        "avg_latency_ms": totals.avg_latency_ms,
        "prompt_tokens": totals.prompt_tokens,
        "completion_tokens": totals.completion_tokens,
        "cache_read_input_tokens": totals.cache_read_input_tokens,
        "cache_creation_input_tokens": totals.cache_creation_input_tokens,
        "cache_rate": totals.cache_rate,
        "usage_known_requests": totals.usage_known_requests,
        "cache_known_requests": totals.cache_known_requests,
        "cost": totals.cost,
        "cost_known_requests": totals.cost_known_requests,
    })
}

/// Per (provider, model) breakdown, sorted by prompt+completion tokens descending. Carries
/// every total except `avg_latency_ms`.
pub fn model_breakdown(rows: &[UsageLogRow]) -> Vec<Value> {
    // The key is only ever a grouping identity, never parsed back apart, so no separator can
    // collide with an upstream model id.
    let mut groups: IndexMap<String, (String, String, Vec<UsageLogRow>)> = IndexMap::new();
    for row in rows {
        let key = format!("{}\u{0}{}", row.provider, row.model);
        groups
            .entry(key)
            .or_insert_with(|| (row.provider.clone(), row.model.clone(), Vec::new()))
            .2
            .push(row.clone());
    }
    let mut out: Vec<(i64, Value)> = Vec::with_capacity(groups.len());
    for (_, (provider, model, group_rows)) in groups {
        let totals = accumulate(&group_rows);
        let mut value = json!({ "provider": provider, "model": model });
        let object = value.as_object_mut().expect("object");
        let rest = totals_json(&totals);
        for (k, v) in rest.as_object().expect("object") {
            if k == "avg_latency_ms" {
                continue;
            }
            object.insert(k.clone(), v.clone());
        }
        out.push((totals.prompt_tokens + totals.completion_tokens, value));
    }
    // A stable sort keeps insertion order among ties, as Array#sort does in V8.
    out.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    out.into_iter().map(|(_, v)| v).collect()
}

/// Hour bucket (`YYYY-MM-DDTHH`) for `grain=hour`, day bucket (`YYYY-MM-DD`) otherwise, in the
/// caller's calendar.
///
/// `offset_minutes` is the client's minutes east of UTC. The range picker selects a *local*
/// day / week / month, so its `from`..`to` lands on local midnights — which in UTC straddle a
/// day boundary for every browser off UTC. Slicing the raw UTC string there spreads a 7-day
/// week over eight keys, and the chart's fixed-width grid then silently drops one end while
/// the totals still count it. 0 = UTC, which is what the legacy `days=` window uses.
pub fn bucket_key(created_at: &str, grain: Grain, offset_minutes: i64) -> String {
    let iso = if offset_minutes == 0 {
        created_at.to_string()
    } else {
        match parse_iso_ms(created_at) {
            Some(ms) => iso_from_ms(ms + offset_minutes * 60_000),
            None => created_at.to_string(),
        }
    };
    let take = if grain == Grain::Hour { 13 } else { 10 };
    iso.chars().take(take).collect()
}

/// Sparse per-(bucket, provider, model) series — only groups that actually have a row,
/// ascending by bucket, then provider, then model. A bucket's totals are the client-side sum
/// over its model points (docs/admin-ui.md § Series shape).
pub fn build_series(rows: &[UsageLogRow], grain: Grain, offset_minutes: i64) -> Vec<Value> {
    #[derive(Default)]
    struct Point {
        bucket: String,
        provider: String,
        model: String,
        requests: i64,
        prompt_tokens: i64,
        completion_tokens: i64,
        cache_read_input_tokens: i64,
        cache_known_requests: i64,
        cost: Option<f64>,
    }
    let mut groups: IndexMap<String, Point> = IndexMap::new();
    for row in rows {
        let bucket = bucket_key(&row.created_at, grain, offset_minutes);
        let key = format!("{bucket} {} {}", row.provider, row.model);
        let point = groups.entry(key).or_insert_with(|| Point {
            bucket,
            provider: row.provider.clone(),
            model: row.model.clone(),
            ..Point::default()
        });
        point.requests += 1;
        if let Some(prompt) = row.prompt_tokens {
            point.prompt_tokens += prompt;
        }
        if let Some(completion) = row.completion_tokens {
            point.completion_tokens += completion;
        }
        if let Some(cache_read) = row.cache_read_input_tokens {
            point.cache_read_input_tokens += cache_read;
            point.cache_known_requests += 1;
        }
        if let Some(cost) = row.cost {
            point.cost = Some(point.cost.unwrap_or(0.0) + cost);
        }
    }
    let mut points: Vec<Point> = groups.into_values().collect();
    points.sort_by(|a, b| {
        a.bucket.cmp(&b.bucket).then_with(|| a.provider.cmp(&b.provider)).then_with(|| a.model.cmp(&b.model))
    });
    points
        .into_iter()
        .map(|p| {
            json!({
                "bucket": p.bucket,
                "provider": p.provider,
                "model": p.model,
                "requests": p.requests,
                "prompt_tokens": p.prompt_tokens,
                "completion_tokens": p.completion_tokens,
                "cache_read_input_tokens": p.cache_read_input_tokens,
                "cache_known_requests": p.cache_known_requests,
                "cost": p.cost,
            })
        })
        .collect()
}

/// Scope the summary to providers that still exist: the builtins plus the user's live custom
/// and CLI slugs (docs/admin-ui.md § Overview page). Rows from a deleted endpoint, and junk
/// prefixes recorded by invalid-model 400s ("unknown", a typo'd prefix), would otherwise haunt
/// the dashboard forever.
pub fn filter_to_live_providers(rows: Vec<UsageLogRow>, custom_slugs: &[String]) -> Vec<UsageLogRow> {
    let builtins: Vec<&str> = PROVIDERS.iter().map(|p| p.as_str()).collect();
    rows.into_iter()
        .filter(|r| builtins.contains(&r.provider.as_str()) || custom_slugs.iter().any(|s| s == &r.provider))
        .collect()
}

/// A log row that can be priced at read time — implemented for both the usage and the logs
/// row shapes, so one `fill_estimated_costs` serves both surfaces as the TypeScript's shared
/// `fillEstimatedCosts` did.
pub trait CostRow {
    fn model(&self) -> &str;
    fn usage(&self) -> CostUsage;
    fn cost(&self) -> Option<f64>;
    fn set_cost(&mut self, cost: f64);
}

impl CostRow for UsageLogRow {
    fn model(&self) -> &str {
        &self.model
    }
    fn usage(&self) -> CostUsage {
        CostUsage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
        }
    }
    fn cost(&self) -> Option<f64> {
        self.cost
    }
    fn set_cost(&mut self, cost: f64) {
        self.cost = Some(cost);
    }
}

impl CostRow for RequestLogPageRow {
    fn model(&self) -> &str {
        &self.model
    }
    fn usage(&self) -> CostUsage {
        CostUsage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
        }
    }
    fn cost(&self) -> Option<f64> {
        self.cost
    }
    fn set_cost(&mut self, cost: f64) {
        self.cost = Some(cost);
    }
}

/// Fill NULL costs at read time with the same resolver used at write time, so pre-migration
/// history still prices (docs/pricing.md). A stored cost is never overwritten; a row that
/// stays unpriced stays NULL.
pub fn fill_estimated_costs<T: CostRow>(rows: &mut [T], table: Option<&PriceTable>) {
    let Some(table) = table else { return };
    for row in rows.iter_mut() {
        if row.cost().is_some() {
            continue;
        }
        if let Some(cost) = estimate_cost(table, row.model(), &row.usage()) {
            row.set_cost(cost);
        }
    }
}

/// The admin surfaces may fetch the price table inline: a missing or legacy (untagged)
/// snapshot is refreshed here, never on the proxy hot path.
pub(crate) async fn read_time_price_table(state: &AppState) -> Option<PriceTable> {
    let table = get_price_table(state).await;
    match table {
        Some(table) if has_source_tagged_price_tables(state.cache()) => Some(table),
        _ => refresh_price_table(state).await,
    }
}

/// Pure aggregation, exported for direct unit testing. `rows` must already be scoped to one
/// user and to `created_at >= from AND created_at <= to`.
pub fn summarize_usage_rows(
    rows: &[UsageLogRow],
    days: i64,
    from: &str,
    to: Option<&str>,
    grain: Option<Grain>,
    offset_minutes: i64,
) -> Value {
    let actual_grain = grain.unwrap_or(if days == 1 { Grain::Hour } else { Grain::Day });
    json!({
        "days": days,
        "from": from,
        "to": to.map(str::to_string).unwrap_or_else(|| iso_from_ms(crate::app::now_ms())),
        "grain": actual_grain.as_str(),
        // Echoed so the client zero-fills its grid in the same calendar the rows were
        // bucketed in; a client that sent no offset reads back 0.
        "offset": offset_minutes,
        "totals": totals_json(&accumulate(rows)),
        "models": model_breakdown(rows),
        "series": build_series(rows, actual_grain, offset_minutes),
    })
}

/// `Number(value)` on a query parameter: JavaScript's numeric coercion, where an empty or
/// blank string is 0 and anything non-numeric is NaN (`None` here).
fn js_number(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    trimmed.parse::<f64>().ok().filter(|v| v.is_finite())
}

struct Window {
    days: i64,
    from: String,
    to: String,
    grain: Grain,
}

/// Both parameter forms (docs/admin-ui.md § Overview page): an explicit `from`/`to` range, or
/// the legacy trailing `days=` window.
fn resolve_window(params: &HashMap<String, String>, now_ms: i64) -> Result<Window, &'static str> {
    match params.get("from") {
        Some(from_param) => {
            let from_ms = parse_iso_ms(from_param).ok_or("invalid_from")?;
            let to_ms = match params.get("to") {
                Some(to_param) => parse_iso_ms(to_param).ok_or("invalid_to")?,
                None => now_ms,
            };
            if from_ms > to_ms {
                return Err("invalid_range");
            }
            if to_ms - from_ms > MAX_SPAN_MS {
                return Err("range_too_large");
            }
            let grain = match params.get("grain").map(String::as_str) {
                None => {
                    if to_ms - from_ms <= 36 * 3_600_000 {
                        Grain::Hour
                    } else {
                        Grain::Day
                    }
                }
                Some("hour") => Grain::Hour,
                Some("day") => Grain::Day,
                Some(_) => return Err("invalid_grain"),
            };
            Ok(Window {
                days: js_round((to_ms - from_ms) as f64 / DAY_MS as f64).max(1),
                from: iso_from_ms(from_ms),
                to: iso_from_ms(to_ms),
                grain,
            })
        }
        None => {
            let days = match params.get("days") {
                None => 7,
                Some(value) => match js_number(value) {
                    Some(v) if v.fract() == 0.0 => v as i64,
                    _ => return Err("invalid_days"),
                },
            };
            if !ALLOWED_DAYS.contains(&days) {
                return Err("invalid_days");
            }
            Ok(Window {
                days,
                from: iso_from_ms(now_ms - days * DAY_MS),
                to: iso_from_ms(now_ms),
                grain: if days == 1 { Grain::Hour } else { Grain::Day },
            })
        }
    }
}

async fn summary(
    State(state): State<AppState>,
    session: SessionUser,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // Bucket calendar, minutes east of UTC. The widest real zone is UTC+14, and
    // half-hour/45-minute zones are why this is minutes rather than hours.
    let offset_minutes = match params.get("offset") {
        None => 0,
        Some(value) => match js_number(value) {
            Some(v) if v.fract() == 0.0 && v.abs() <= 840.0 => v as i64,
            _ => return error(StatusCode::BAD_REQUEST, "invalid_offset"),
        },
    };

    let window = match resolve_window(&params, state.now_ms()) {
        Ok(window) => window,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };

    let Ok(rows) = usage_rows_in_range(state.pool(), &session.user.id, &window.from, &window.to).await else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    };

    // Live slugs cover both user-defined kinds — a CLI provider's traffic is as real as a
    // custom endpoint's (docs/cli.md), and the filter would otherwise silently drop it from
    // every Overview total.
    let mut slugs: Vec<String> =
        list_custom_providers(state.pool(), &session.user.id).await.unwrap_or_default().into_iter().map(|p| p.slug).collect();
    slugs.extend(list_cli_providers(state.pool(), &session.user.id).await.unwrap_or_default().into_iter().map(|p| p.slug));
    let mut scoped = filter_to_live_providers(rows, &slugs);

    let table = read_time_price_table(&state).await;
    fill_estimated_costs(&mut scoped, table.as_ref());
    Json(summarize_usage_rows(&scoped, window.days, &window.from, Some(&window.to), Some(window.grain), offset_minutes))
        .into_response()
}

/// A `ProviderId` check for the Logs page's `usage_type` — kept here beside the live-provider
/// filter so both surfaces agree on what "builtin" means.
pub(crate) fn is_builtin_provider(provider: &str) -> bool {
    ProviderId::parse(provider).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::request_logs::{insert_request_log, RequestLogEntry};
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_router, test_state};
    use crate::pricing::litellm::trim_litellm_table;
    use crate::upstream::{MockTransport, UpstreamResponse};
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn row(overrides: Value) -> UsageLogRow {
        let mut base = json!({
            "provider": "claude-code",
            "model": "claude-code/claude-opus-5",
            "status_code": 200,
            "latency_ms": 100,
            "prompt_tokens": null,
            "completion_tokens": null,
            "cache_read_input_tokens": null,
            "cache_creation_input_tokens": null,
            "cost": null,
            "created_at": "2026-08-02T10:00:00.000Z",
        });
        for (k, v) in overrides.as_object().expect("overrides object") {
            base[k] = v.clone();
        }
        serde_json::from_value(base).expect("a usage row")
    }

    fn summary_of(rows: &[UsageLogRow], days: i64) -> Value {
        summarize_usage_rows(rows, days, "from", None, None, 0)
    }

    #[test]
    fn counts_requests_and_errors_over_status_400_and_up() {
        let rows = [row(json!({ "status_code": 200 })), row(json!({ "status_code": 404 })), row(json!({ "status_code": 500 }))];
        let out = summary_of(&rows, 7);
        assert_eq!(out["totals"]["requests"], 3);
        assert_eq!(out["totals"]["errors"], 2);
    }

    #[test]
    fn averages_latency_rounded_and_null_for_zero_rows() {
        let rows = [row(json!({ "latency_ms": 100 })), row(json!({ "latency_ms": 150 })), row(json!({ "latency_ms": 151 }))];
        // 401/3 = 133.67 -> 134
        assert_eq!(summary_of(&rows, 7)["totals"]["avg_latency_ms"], 134);
        let empty = summary_of(&[], 7);
        assert_eq!(empty["totals"]["avg_latency_ms"], Value::Null);
        assert_eq!(empty["totals"]["requests"], 0);
    }

    #[test]
    fn sums_tokens_over_non_null_rows_only() {
        let rows = [
            row(json!({ "prompt_tokens": 100, "completion_tokens": 50 })),
            row(json!({ "prompt_tokens": 200 })),
            // A count_tokens-shaped row: usage wholly unknown, excluded rather than zero-summed.
            row(json!({})),
        ];
        let out = summary_of(&rows, 7);
        assert_eq!(out["totals"]["prompt_tokens"], 300);
        assert_eq!(out["totals"]["completion_tokens"], 50);
        assert_eq!(out["totals"]["usage_known_requests"], 2);
    }

    #[test]
    fn cache_rate_covers_only_rows_with_a_known_cache_read_and_a_positive_prompt() {
        let rows = [
            row(json!({ "prompt_tokens": 100, "cache_read_input_tokens": 20 })),
            row(json!({ "prompt_tokens": 200, "cache_read_input_tokens": 50 })),
            row(json!({ "prompt_tokens": 50 })),
            row(json!({ "prompt_tokens": 0, "cache_read_input_tokens": 10 })),
        ];
        let out = summary_of(&rows, 7);
        // The token total sums every non-null row unconditionally, the 0-prompt row included.
        assert_eq!(out["totals"]["cache_read_input_tokens"], 80);
        let rate = out["totals"]["cache_rate"].as_f64().unwrap();
        assert!((rate - 70.0 / 300.0).abs() < 1e-12);
        assert_eq!(out["totals"]["cache_known_requests"], 3);

        let none = summary_of(&[row(json!({ "prompt_tokens": 100 }))], 7);
        assert_eq!(none["totals"]["cache_rate"], Value::Null);
    }

    #[test]
    fn models_group_by_provider_and_model_only() {
        let rows = [
            row(json!({ "prompt_tokens": 10, "completion_tokens": 5 })),
            row(json!({ "provider": "grok", "model": "grok/grok-4.5", "prompt_tokens": 100, "completion_tokens": 50 })),
            row(json!({ "prompt_tokens": 20, "completion_tokens": 5 })),
            row(json!({ "prompt_tokens": 5, "completion_tokens": 5 })),
        ];
        let out = summary_of(&rows, 7);
        let models = out["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["provider"], "grok");
        assert_eq!(models[0]["requests"], 1);
        assert_eq!(models[1]["model"], "claude-code/claude-opus-5");
        assert_eq!(models[1]["requests"], 3);
        assert_eq!(models[1]["prompt_tokens"], 35);
        assert_eq!(models[1]["completion_tokens"], 15);
        for model in models {
            // Never per-account or per-alias: the breakdown is model identity only.
            assert!(model.get("account_id").is_none());
            assert!(model.get("group_name").is_none());
            assert!(model.get("avg_latency_ms").is_none());
        }
    }

    #[test]
    fn series_buckets_by_hour_for_one_day_and_by_day_otherwise() {
        let rows = [
            row(json!({ "created_at": "2026-08-02T10:15:00.000Z", "prompt_tokens": 10, "completion_tokens": 1 })),
            row(json!({ "created_at": "2026-08-02T10:45:00.000Z", "prompt_tokens": 20, "completion_tokens": 2 })),
            row(json!({ "created_at": "2026-08-02T12:00:00.000Z", "prompt_tokens": 30, "completion_tokens": 3 })),
        ];
        let hourly = summary_of(&rows, 1);
        assert_eq!(
            hourly["series"],
            json!([
                { "bucket": "2026-08-02T10", "provider": "claude-code", "model": "claude-code/claude-opus-5", "requests": 2, "prompt_tokens": 30, "completion_tokens": 3, "cache_read_input_tokens": 0, "cache_known_requests": 0, "cost": null },
                { "bucket": "2026-08-02T12", "provider": "claude-code", "model": "claude-code/claude-opus-5", "requests": 1, "prompt_tokens": 30, "completion_tokens": 3, "cache_read_input_tokens": 0, "cache_known_requests": 0, "cost": null },
            ])
        );
        let daily = summary_of(&rows, 7);
        assert_eq!(
            daily["series"],
            json!([
                { "bucket": "2026-08-02", "provider": "claude-code", "model": "claude-code/claude-opus-5", "requests": 3, "prompt_tokens": 60, "completion_tokens": 6, "cache_read_input_tokens": 0, "cache_known_requests": 0, "cost": null },
            ])
        );
    }

    #[test]
    fn series_is_sparse_ascending_and_split_per_model() {
        let sparse = summary_of(
            &[row(json!({ "created_at": "2026-08-03T00:00:00.000Z" })), row(json!({ "created_at": "2026-08-01T00:00:00.000Z" }))],
            7,
        );
        assert_eq!(
            sparse["series"].as_array().unwrap().iter().map(|p| p["bucket"].clone()).collect::<Vec<_>>(),
            vec![json!("2026-08-01"), json!("2026-08-03")]
        );

        let rows = [
            row(json!({ "prompt_tokens": 10, "completion_tokens": 1 })),
            row(json!({ "model": "claude-code/claude-sonnet-5", "prompt_tokens": 20, "completion_tokens": 2 })),
            row(json!({ "prompt_tokens": 30, "completion_tokens": 3 })),
            row(json!({ "provider": "grok", "model": "grok/grok-4.5", "prompt_tokens": 40, "completion_tokens": 4 })),
        ];
        let out = summary_of(&rows, 7);
        let series = out["series"].as_array().unwrap();
        assert_eq!(
            series.iter().map(|p| (p["model"].clone(), p["requests"].clone(), p["prompt_tokens"].clone())).collect::<Vec<_>>(),
            vec![
                (json!("claude-code/claude-opus-5"), json!(2), json!(40)),
                (json!("claude-code/claude-sonnet-5"), json!(1), json!(20)),
                (json!("grok/grok-4.5"), json!(1), json!(40)),
            ]
        );
        // Bucket totals are the client-side sum over its model points.
        let sum: i64 = series.iter().map(|p| p["prompt_tokens"].as_i64().unwrap()).sum();
        assert_eq!(sum, 100);
    }

    #[test]
    fn cache_known_requests_separates_zero_cached_from_unreported() {
        let rows = [
            row(json!({ "prompt_tokens": 100, "cache_read_input_tokens": 25 })),
            row(json!({ "prompt_tokens": 100, "cache_read_input_tokens": 0 })),
            row(json!({ "prompt_tokens": 100 })),
        ];
        let out = summary_of(&rows, 7);
        assert_eq!(out["series"].as_array().unwrap().len(), 1);
        assert_eq!(out["series"][0]["requests"], 3);
        assert_eq!(out["series"][0]["cache_known_requests"], 2);
        assert_eq!(out["series"][0]["cache_read_input_tokens"], 25);
    }

    #[test]
    fn echoes_days_and_from_verbatim() {
        let out = summarize_usage_rows(&[], 30, "2026-07-03T00:00:00.000Z", None, None, 0);
        assert_eq!(out["days"], 30);
        assert_eq!(out["from"], "2026-07-03T00:00:00.000Z");
    }

    #[test]
    fn live_provider_filter_keeps_builtins_and_given_slugs_only() {
        let rows = vec![
            row(json!({ "provider": "claude-code" })),
            row(json!({ "provider": "codex" })),
            row(json!({ "provider": "grok" })),
            row(json!({ "provider": "live-slug" })),
            row(json!({ "provider": "dead-slug" })),
            row(json!({ "provider": "unknown" })),
        ];
        let out = filter_to_live_providers(rows, &["live-slug".to_string()]);
        assert_eq!(out.iter().map(|r| r.provider.clone()).collect::<Vec<_>>(), ["claude-code", "codex", "grok", "live-slug"]);
    }

    #[test]
    fn read_time_pricing_never_overwrites_a_stored_cost() {
        let table = trim_litellm_table(&json!({
            "claude-opus-5": { "input_cost_per_token": 0.00001, "output_cost_per_token": 0.00005 }
        }));
        let mut rows = vec![
            row(json!({ "prompt_tokens": 1000, "completion_tokens": 100 })),
            row(json!({ "prompt_tokens": 1000, "completion_tokens": 100, "cost": 42 })),
            row(json!({ "model": "claude-code/unpriced-model", "prompt_tokens": 1000 })),
        ];
        fill_estimated_costs(&mut rows, Some(&table));
        assert!((rows[0].cost.unwrap() - (1000.0 * 0.00001 + 100.0 * 0.00005)).abs() < 1e-12);
        assert_eq!(rows[1].cost, Some(42.0));
        assert_eq!(rows[2].cost, None);

        // No table at all leaves every row untouched.
        let mut untouched = vec![row(json!({ "prompt_tokens": 1000, "completion_tokens": 100 }))];
        fill_estimated_costs(&mut untouched, None);
        assert_eq!(untouched[0].cost, None);
    }

    // ---- route tests ----

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// A transport that answers every upstream attempt (the read-time price fetch) with a 500,
    /// so no test ever reaches the real network. Enough handlers are queued for the price
    /// refresh both sources make on every admin request in the test.
    fn offline() -> std::sync::Arc<MockTransport> {
        let mock = MockTransport::new();
        for _ in 0..64 {
            mock.expect(|_| {
                Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, http::HeaderMap::new(), "offline"))
            });
        }
        mock
    }

    async fn signed_in(state: &AppState, email: &str) -> (String, String) {
        let user = insert_user(state.pool(), email).await;
        let (_, cookie) = crate::auth::session::create_session(state, &user.id, false).await.unwrap();
        (user.id, cookie.split(';').next().unwrap().to_string())
    }

    async fn seed_log(state: &AppState, user_id: &str, overrides: Value) {
        let mut base = json!({
            "user_id": user_id,
            "provider": "claude-code",
            "model": "claude-code/claude-opus-5",
            "status_code": 200,
            "latency_ms": 100,
            "started_at": crate::ids::now_iso(),
        });
        for (k, v) in overrides.as_object().expect("overrides object") {
            base[k] = v.clone();
        }
        let created_at = base.as_object_mut().unwrap().remove("created_at");
        let entry: RequestLogEntry = serde_json::from_value(base).expect("a log entry");
        let id = insert_request_log(state.pool(), &entry).await.unwrap();
        if let Some(Value::String(created_at)) = created_at {
            sqlx::query("UPDATE request_logs SET created_at = $1 WHERE id = $2")
                .bind(created_at)
                .bind(&id)
                .execute(state.pool())
                .await
                .unwrap();
        }
    }

    async fn seed_custom_provider(state: &AppState, user_id: &str, slug: &str) {
        crate::db::custom_providers::insert_custom_provider(
            state.pool(),
            crate::db::custom_providers::NewCustomProvider {
                user_id,
                slug,
                name: slug,
                format: "openai",
                base_url: "https://api.example.com/v1",
                count_tokens_url: None,
                models_mode: "auto",
                manual_models_json: None,
            },
        )
        .await
        .expect("insert custom provider")
        .expect("slug is free");
    }

    async fn get(state: &AppState, cookie: &str, uri: &str) -> Response {
        test_router(state.clone())
            .oneshot(Request::builder().uri(uri).header(header::COOKIE, cookie).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_summary_requires_a_session_and_validates_days() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let response = test_router(state.clone())
            .oneshot(Request::builder().uri("/api/usage/summary").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let (_, cookie) = signed_in(&state, "usage@example.com").await;
        for bad in ["days=3", "days=abc"] {
            let response = get(&state, &cookie, &format!("/api/usage/summary?{bad}")).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(body_json(response).await, json!({ "error": "invalid_days" }));
        }
        // No parameter at all is the 7-day window; 1 and 30 are the other allowed values.
        let json = body_json(get(&state, &cookie, "/api/usage/summary").await).await;
        assert_eq!(json["days"], 7);
        assert_eq!(json["offset"], 0, "the legacy window keeps UTC keys");
        for days in [1, 30] {
            let json = body_json(get(&state, &cookie, &format!("/api/usage/summary?days={days}")).await).await;
            assert_eq!(json["days"], days);
        }
    }

    #[tokio::test]
    async fn the_summary_is_scoped_to_the_viewer_and_the_window() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "scope@example.com").await;
        let other = insert_user(state.pool(), "other@example.com").await;
        let now = state.now_ms();
        seed_log(&state, &user_id, json!({ "prompt_tokens": 10, "completion_tokens": 1, "created_at": iso_from_ms(now - 6 * DAY_MS) })).await;
        seed_log(&state, &user_id, json!({ "prompt_tokens": 999, "created_at": iso_from_ms(now - 8 * DAY_MS) })).await;
        seed_log(&state, &other.id, json!({ "prompt_tokens": 999, "created_at": iso_from_ms(now - DAY_MS) })).await;

        let json = body_json(get(&state, &cookie, "/api/usage/summary?days=7").await).await;
        assert_eq!(json["totals"]["requests"], 1, "another user's rows and rows before `from` never count");
        assert_eq!(json["totals"]["prompt_tokens"], 10);
    }

    #[tokio::test]
    async fn end_to_end_totals_models_and_series() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "e2e@example.com").await;
        let day1 = state.now_ms() - 3 * DAY_MS;
        let day2 = day1 + DAY_MS;
        seed_log(
            &state,
            &user_id,
            json!({ "latency_ms": 100, "prompt_tokens": 100, "completion_tokens": 40, "cache_read_input_tokens": 20,
                    "cache_creation_input_tokens": 0, "created_at": iso_from_ms(day1) }),
        )
        .await;
        seed_log(
            &state,
            &user_id,
            json!({ "provider": "grok", "model": "grok/grok-4.5", "status_code": 500, "latency_ms": 300,
                    "error_code": "upstream_error", "created_at": iso_from_ms(day1 + 1_800_000) }),
        )
        .await;
        seed_log(&state, &user_id, json!({ "latency_ms": 200, "created_at": iso_from_ms(day2) })).await;

        let json = body_json(get(&state, &cookie, "/api/usage/summary?days=7").await).await;
        assert_eq!(json["totals"]["requests"], 3);
        assert_eq!(json["totals"]["errors"], 1);
        assert_eq!(json["totals"]["avg_latency_ms"], 200);
        assert_eq!(json["totals"]["prompt_tokens"], 100);
        assert_eq!(json["totals"]["completion_tokens"], 40);
        assert_eq!(json["totals"]["cache_read_input_tokens"], 20);
        assert_eq!(json["totals"]["cache_rate"], 0.2);
        assert_eq!(json["totals"]["usage_known_requests"], 1);
        assert_eq!(json["totals"]["cache_known_requests"], 1);
        assert_eq!(json["models"].as_array().unwrap().len(), 2);
        let series = json["series"].as_array().unwrap();
        // day1 holds two models -> two points, and day2 one.
        assert_eq!(series.len(), 3);
        assert_eq!(series[0]["bucket"], bucket_key(&iso_from_ms(day1), Grain::Day, 0));
        assert_eq!(series[1]["provider"], "grok");
        assert_eq!(series[2]["bucket"], bucket_key(&iso_from_ms(day2), Grain::Day, 0));
    }

    #[tokio::test]
    async fn only_live_providers_reach_the_dashboard() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "live@example.com").await;
        seed_custom_provider(&state, &user_id, "my-endpoint").await;
        sqlx::query(
            "INSERT INTO cli_providers (id, user_id, device_id, slug, name, format, sort_order, created_at, updated_at)
             VALUES ('cliprov_1', $1, NULL, 'my-mac', 'My Mac', 'openai', 0, $2, $2)",
        )
        .bind(&user_id)
        .bind(crate::ids::now_iso())
        .execute(state.pool())
        .await
        .unwrap();

        seed_log(&state, &user_id, json!({ "prompt_tokens": 10 })).await;
        seed_log(&state, &user_id, json!({ "provider": "my-endpoint", "model": "my-endpoint/x", "prompt_tokens": 20 })).await;
        seed_log(&state, &user_id, json!({ "provider": "my-mac", "model": "my-mac/llama3", "prompt_tokens": 7 })).await;
        // A deleted endpoint's slug and an invalid-model 400's "unknown" prefix both drop out.
        seed_log(&state, &user_id, json!({ "provider": "deleted-endpoint", "model": "deleted-endpoint/x" })).await;
        seed_log(&state, &user_id, json!({ "provider": "unknown", "model": "gibberish" })).await;

        let json = body_json(get(&state, &cookie, "/api/usage/summary?days=30").await).await;
        assert_eq!(json["totals"]["requests"], 3);
        assert_eq!(json["totals"]["prompt_tokens"], 37);
        let mut providers: Vec<String> =
            json["models"].as_array().unwrap().iter().map(|m| m["provider"].as_str().unwrap().to_string()).collect();
        providers.sort();
        assert_eq!(providers, ["claude-code", "my-endpoint", "my-mac"]);
    }

    #[tokio::test]
    async fn cost_totals_come_from_stored_costs_and_read_time_pricing() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "cost@example.com").await;
        seed_log(&state, &user_id, json!({ "prompt_tokens": 100, "completion_tokens": 10, "cost": 1.5 })).await;
        seed_log(&state, &user_id, json!({ "prompt_tokens": 100, "completion_tokens": 10, "cost": 0.5 })).await;
        seed_log(&state, &user_id, json!({ "prompt_tokens": 100, "completion_tokens": 10 })).await;

        let json = body_json(get(&state, &cookie, "/api/usage/summary?days=30").await).await;
        assert!((json["totals"]["cost"].as_f64().unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(json["totals"]["cost_known_requests"], 2, "an unpriced row contributes nothing");
    }

    #[tokio::test]
    async fn a_legacy_untagged_snapshot_is_refreshed_from_the_admin_summary() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        // Both halves of the refresh: the OpenRouter catalog and the LiteLLM table.
        for _ in 0..8 {
            mock.expect(|request| {
                if request.url.contains("openrouter.ai") {
                    Ok(UpstreamResponse::json(
                        StatusCode::OK,
                        &json!({ "data": [{ "id": "z-ai/glm-5.2", "pricing": { "prompt": "0.000001", "completion": "0.000002" } }] }),
                    ))
                } else {
                    Ok(UpstreamResponse::json(
                        StatusCode::OK,
                        &json!({ "claude-opus-5": { "input_cost_per_token": 0.00001, "output_cost_per_token": 0.00005 } }),
                    ))
                }
            });
        }
        let state = test_state(pool, mock.clone());
        let (user_id, cookie) = signed_in(&state, "legacy@example.com").await;
        seed_custom_provider(&state, &user_id, "openrouter").await;
        seed_log(
            &state,
            &user_id,
            json!({ "provider": "openrouter", "model": "openrouter/z-ai/glm-5.2", "prompt_tokens": 100, "completion_tokens": 10 }),
        )
        .await;

        let json = body_json(get(&state, &cookie, "/api/usage/summary?days=30").await).await;
        let expected = 100.0 * 0.000001 + 10.0 * 0.000002;
        assert!((json["totals"]["cost"].as_f64().unwrap() - expected).abs() < 1e-12);
        assert_eq!(json["totals"]["cost_known_requests"], 1);
        assert_eq!(mock.requests().len(), 2, "both price sources are fetched once");
    }

    #[tokio::test]
    async fn from_to_ranges_choose_and_validate_the_grain() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "range@example.com").await;
        for (created_at, prompt) in [
            ("2026-08-15T08:30:00.000Z", 100),
            ("2026-08-15T14:45:00.000Z", 200),
            ("2026-08-16T10:00:00.000Z", 300),
        ] {
            seed_log(&state, &user_id, json!({ "prompt_tokens": prompt, "created_at": created_at })).await;
        }

        let json = body_json(
            get(&state, &cookie, "/api/usage/summary?from=2026-08-15T00:00:00.000Z&to=2026-08-15T23:59:59.999Z&grain=hour").await,
        )
        .await;
        assert_eq!(json["grain"], "hour");
        assert_eq!(json["totals"]["requests"], 2);
        assert_eq!(json["totals"]["prompt_tokens"], 300);
        assert_eq!(
            json["series"].as_array().unwrap().iter().map(|p| p["bucket"].clone()).collect::<Vec<_>>(),
            vec![json!("2026-08-15T08"), json!("2026-08-15T14")]
        );

        let json = body_json(
            get(&state, &cookie, "/api/usage/summary?from=2026-08-15T00:00:00.000Z&to=2026-08-16T23:59:59.999Z&grain=day").await,
        )
        .await;
        assert_eq!(json["grain"], "day");
        assert_eq!(json["totals"]["requests"], 3);
        assert_eq!(json["totals"]["prompt_tokens"], 600);
        assert_eq!(
            json["series"].as_array().unwrap().iter().map(|p| p["bucket"].clone()).collect::<Vec<_>>(),
            vec![json!("2026-08-15"), json!("2026-08-16")]
        );

        // An explicit grain wins over the span heuristic; `days` still reads 1 here, which is
        // why the client must prefer `grain`.
        let json = body_json(
            get(&state, &cookie, "/api/usage/summary?from=2026-08-15T00:00:00.000Z&to=2026-08-15T23:59:59.999Z&grain=day").await,
        )
        .await;
        assert_eq!(json["days"], 1);
        assert_eq!(json["grain"], "day");
        assert_eq!(json["series"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn from_to_grain_and_offset_are_validated() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (_, cookie) = signed_in(&state, "validate@example.com").await;
        for (uri, expected) in [
            ("/api/usage/summary?from=invalid-date", "invalid_from"),
            ("/api/usage/summary?from=2026-08-15T00:00:00Z&to=invalid-to", "invalid_to"),
            ("/api/usage/summary?from=2026-08-20T00:00:00Z&to=2026-08-15T00:00:00Z", "invalid_range"),
            ("/api/usage/summary?from=2026-08-15T00:00:00Z&to=2026-08-16T00:00:00Z&grain=minute", "invalid_grain"),
            ("/api/usage/summary?from=2026-08-15T00:00:00Z&to=2026-08-16T00:00:00Z&offset=900", "invalid_offset"),
            ("/api/usage/summary?from=2026-08-15T00:00:00Z&to=2026-08-16T00:00:00Z&offset=30.5", "invalid_offset"),
            ("/api/usage/summary?from=2020-01-01T00:00:00Z&to=2026-08-16T00:00:00Z", "range_too_large"),
        ] {
            let response = get(&state, &cookie, uri).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body_json(response).await, json!({ "error": expected }), "{uri}");
        }
    }

    #[tokio::test]
    async fn buckets_land_in_the_clients_calendar() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "offset@example.com").await;
        // Mon 2026-08-10 .. Sun 2026-08-16 local in UTC+8 (offset 480): local Monday 00:00 is
        // Sunday 16:00Z, so the span touches eight UTC dates and UTC bucketing would spill.
        for created_at in ["2026-08-09T16:30:00.000Z", "2026-08-13T02:00:00.000Z", "2026-08-16T09:00:00.000Z"] {
            seed_log(&state, &user_id, json!({ "created_at": created_at })).await;
        }
        let json = body_json(
            get(
                &state,
                &cookie,
                "/api/usage/summary?from=2026-08-09T16:00:00.000Z&to=2026-08-16T15:59:59.999Z&grain=day&offset=480",
            )
            .await,
        )
        .await;
        assert_eq!(json["offset"], 480);
        assert_eq!(json["totals"]["requests"], 3);
        assert_eq!(
            json["series"].as_array().unwrap().iter().map(|p| p["bucket"].clone()).collect::<Vec<_>>(),
            vec![json!("2026-08-10"), json!("2026-08-13"), json!("2026-08-16")]
        );
    }

    #[tokio::test]
    async fn hour_buckets_honor_half_hour_offsets() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "halfhour@example.com").await;
        // UTC+5:30 (offset 330): local 2026-08-15 runs 2026-08-14T18:30Z .. 18:29Z.
        for created_at in ["2026-08-14T18:45:00.000Z", "2026-08-15T15:20:00.000Z"] {
            seed_log(&state, &user_id, json!({ "created_at": created_at })).await;
        }
        let json = body_json(
            get(
                &state,
                &cookie,
                "/api/usage/summary?from=2026-08-14T18:30:00.000Z&to=2026-08-15T18:29:59.999Z&grain=hour&offset=330",
            )
            .await,
        )
        .await;
        assert_eq!(
            json["series"].as_array().unwrap().iter().map(|p| p["bucket"].clone()).collect::<Vec<_>>(),
            vec![json!("2026-08-15T00"), json!("2026-08-15T20")]
        );
    }
}
