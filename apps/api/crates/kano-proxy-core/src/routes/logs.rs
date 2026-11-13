//! `/api/logs` — the Logs page's cursor-paged request history, the counts its pickers show
//! and the export (docs/admin-ui.md § Logs page, docs/auth.md, docs/logging.md).
//!
//! Rows carry counts, costs and names only: prompts, completions and tokens never enter
//! `request_logs`, and the `api_keys` id is resolved to a name here so it never leaves the
//! process. A row served by an account that is not the viewer's own is either borrowed
//! through the pool extension (which names the ones it still lets this viewer see) or removed.

use std::collections::{HashMap, HashSet};

use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use indexmap::IndexMap;
use serde_json::{json, Value};

use crate::auth::session::SessionUser;
use crate::db::accounts::{iso_from_ms, parse_iso_ms};
use crate::db::keys::list_keys;
use crate::db::request_logs::{
    facet_rows_in_range, list_request_log_page, viewer_account_labels, FacetLogRow, LogPageQuery, RequestLogPageRow,
};
use crate::pool::extension::SharedLabel;
use crate::pricing::litellm::PriceTable;
use crate::routes::usage::{fill_estimated_costs, is_builtin_provider, js_number, read_time_price_table, MAX_SPAN_MS};
use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(list)).route("/facets", get(facets)).route("/export", get(export))
}

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 100;
/// Rows per export read: enough to keep the query count low, small enough that a year of
/// history never sits in memory at once.
const EXPORT_BATCH: i64 = 500;

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// The page cursor: the last shown row's `(created_at, id)`, base64 of `"<created_at> <id>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub created_at: String,
    pub id: String,
}

pub fn encode_cursor(cursor: &Cursor) -> String {
    BASE64.encode(format!("{} {}", cursor.created_at, cursor.id))
}

/// `None` for anything that is not exactly one base64-encoded `"<created_at> <id>"` pair —
/// a malformed cursor is a 400, never a silently different page.
pub fn decode_cursor(value: &str) -> Option<Cursor> {
    if value.len() % 4 != 0 || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=') {
        return None;
    }
    // Padding may only ever trail the payload.
    if value.trim_end_matches('=').contains('=') || value.len() - value.trim_end_matches('=').len() > 2 {
        return None;
    }
    let bytes = BASE64.decode(value).ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    let separator = decoded.find(' ')?;
    if separator == 0 || Some(separator) != decoded.rfind(' ') {
        return None;
    }
    let id = decoded[separator + 1..].to_string();
    if id.is_empty() {
        return None;
    }
    Some(Cursor { created_at: decoded[..separator].to_string(), id })
}

/// `50` when absent; otherwise a plain decimal integer in `1..=100`. Anything else is a 400,
/// so a typo never silently reads the whole table.
pub fn parse_limit(value: Option<&str>) -> Option<i64> {
    let Some(value) = value else { return Some(DEFAULT_LIMIT) };
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let limit: i64 = value.parse().ok()?;
    (1..=MAX_LIMIT).contains(&limit).then_some(limit)
}

/// The filters `/api/logs` and `/api/logs/export` share, borrowed from the query string.
struct Filters<'a> {
    provider: Option<&'a str>,
    model: Option<&'a str>,
    group: Option<&'a str>,
    direct: bool,
    errors_only: bool,
}

/// `group` names one alias and `direct` asks for no alias, so asking for both is a 400.
fn filters(params: &HashMap<String, String>) -> Result<Filters<'_>, &'static str> {
    let group = params.get("group").map(String::as_str);
    let direct = params.get("direct").map(String::as_str) == Some("1");
    if group.is_some() && direct {
        return Err("invalid_filter");
    }
    Ok(Filters {
        provider: params.get("provider").map(String::as_str),
        model: params.get("model").map(String::as_str),
        group,
        direct,
        errors_only: params.get("errors").map(String::as_str) == Some("1"),
    })
}

/// Required `from`/`to`, normalized to the stored ISO shape so the string comparison in SQL is
/// a time comparison. Wider than a year is refused rather than scanned, as on the summary.
pub fn parse_range(params: &HashMap<String, String>) -> Result<(i64, i64), &'static str> {
    let from = params.get("from").and_then(|v| parse_iso_ms(v)).ok_or("invalid_range")?;
    let to = params.get("to").and_then(|v| parse_iso_ms(v)).ok_or("invalid_range")?;
    if from > to {
        return Err("invalid_range");
    }
    if to - from > MAX_SPAN_MS {
        return Err("range_too_large");
    }
    Ok((from, to))
}

async fn list(State(state): State<AppState>, session: SessionUser, Query(params): Query<HashMap<String, String>>) -> Response {
    let Some(limit) = parse_limit(params.get("limit").map(String::as_str)) else {
        return error(StatusCode::BAD_REQUEST, "invalid_limit");
    };
    let cursor = match params.get("cursor") {
        None => None,
        Some(value) => match decode_cursor(value) {
            Some(cursor) => Some(cursor),
            None => return error(StatusCode::BAD_REQUEST, "invalid_cursor"),
        },
    };
    let filters = match filters(&params) {
        Ok(f) => f,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };

    let query = LogPageQuery {
        user_id: &session.user.id,
        provider: filters.provider,
        model: filters.model,
        group: filters.group,
        direct: filters.direct,
        errors_only: filters.errors_only,
        from: None,
        to: None,
        cursor: cursor.as_ref().map(|c| (c.created_at.as_str(), c.id.as_str())),
        limit: limit + 1,
    };
    let Ok(mut fetched) = list_request_log_page(state.pool(), &query).await else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    };
    let has_next_page = fetched.len() as i64 > limit;
    fetched.truncate(limit as usize);
    let page = fetched;

    // Read-time pricing matches the summary. A missing or legacy table may be refreshed on
    // this admin surface; proxied request handling never fetches it.
    let table = read_time_price_table(&state).await;
    let mut priced = page.clone();
    fill_estimated_costs(&mut priced, table.as_ref());

    let Ok(names) = Names::load(&state, &session.user.id).await else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    };
    let shared = names.shared_labels(&state, &session.user.id, &priced).await;

    let rows: Vec<Value> = priced.iter().map(|row| row_json(row, &names.resolve(row, &shared))).collect();
    let next_cursor = match page.last() {
        Some(last) if has_next_page => {
            Value::String(encode_cursor(&Cursor { created_at: last.created_at.clone(), id: last.id.clone() }))
        }
        _ => Value::Null,
    };
    Json(json!({ "rows": rows, "next_cursor": next_cursor })).into_response()
}

/// The viewer's own account labels and key names, read once per request (or per export).
struct Names {
    account_labels: HashMap<String, Option<String>>,
    api_key_names: HashMap<String, String>,
}

/// A row's display names as the Logs page shows them.
struct Resolved {
    account_label: Option<String>,
    account_removed: bool,
    account_shared_by: Option<String>,
    api_key_name: Option<String>,
    api_key_removed: bool,
}

impl Names {
    async fn load(state: &AppState, user_id: &str) -> Result<Self, sqlx::Error> {
        let account_labels = viewer_account_labels(state.pool(), user_id).await?.into_iter().collect();
        let api_key_names = list_keys(state.pool(), user_id).await?.into_iter().map(|k| (k.id, k.name)).collect();
        Ok(Self { account_labels, api_key_names })
    }

    /// Names for the rows' accounts that are not the viewer's own, asked of the pool extension
    /// once, in row order. A failing extension leaves them reading as removed.
    async fn shared_labels(&self, state: &AppState, user_id: &str, rows: &[RequestLogPageRow]) -> HashMap<String, SharedLabel> {
        let mut seen: HashSet<&str> = HashSet::new();
        let foreign: Vec<String> = rows
            .iter()
            .filter_map(|row| row.account_id.as_deref())
            .filter(|id| !self.account_labels.contains_key(*id) && seen.insert(id))
            .map(str::to_string)
            .collect();
        match state.pool_extension() {
            Some(ext) if !foreign.is_empty() => match ext.label_shared(state, user_id, &foreign).await {
                Ok(labels) => labels,
                Err(error) => {
                    tracing::error!(%error, "pool extension labelShared failed for the logs page");
                    HashMap::new()
                }
            },
            _ => HashMap::new(),
        }
    }

    fn resolve(&self, row: &RequestLogPageRow, shared: &HashMap<String, SharedLabel>) -> Resolved {
        let account_label = match row.account_id.as_deref() {
            None => None,
            // Live label, else the lender's label for a borrowed row, else the name stored when
            // the row was written — removal is the explicit flag, never a null name.
            Some(id) => self
                .account_labels
                .get(id)
                .cloned()
                .flatten()
                .or_else(|| shared.get(id).map(|s| s.label.clone()))
                .or_else(|| row.account_label.clone()),
        };
        let account_removed = row
            .account_id
            .as_deref()
            .is_some_and(|id| !self.account_labels.contains_key(id) && !shared.contains_key(id));
        let account_shared_by = row.account_id.as_deref().and_then(|id| shared.get(id)).map(|s| s.owner_label.clone());
        // The `api_keys` id is resolved to a name here and never leaves the process
        // (docs/admin-ui.md § Logs page): a bare id under "API key" reads as the credential to
        // whoever is looking, so the client gets only the name plus this explicit removed flag.
        let api_key_name = row
            .api_key_id
            .as_deref()
            .and_then(|id| self.api_key_names.get(id).cloned().or_else(|| row.api_key_name.clone()));
        let api_key_removed = row.api_key_id.as_deref().is_some_and(|id| !self.api_key_names.contains_key(id));
        Resolved { account_label, account_removed, account_shared_by, api_key_name, api_key_removed }
    }
}

fn row_json(row: &RequestLogPageRow, names: &Resolved) -> Value {
    json!({
        "id": row.id,
        "created_at": row.created_at,
        "provider": row.provider,
        "model": row.model,
        "group_name": row.group_name,
        "account_id": row.account_id,
        "account_label": names.account_label,
        "account_removed": names.account_removed,
        "account_shared_by": names.account_shared_by,
        "api_key_name": names.api_key_name,
        "api_key_removed": names.api_key_removed,
        "usage_type": if is_builtin_provider(&row.provider) { "oauth" } else { "api" },
        "status_code": row.status_code,
        "upstream_status": row.upstream_status,
        "error_code": row.error_code,
        "latency_ms": row.latency_ms,
        "prompt_tokens": row.prompt_tokens,
        "completion_tokens": row.completion_tokens,
        "cache_read_input_tokens": row.cache_read_input_tokens,
        "cache_creation_input_tokens": row.cache_creation_input_tokens,
        "cost": row.cost,
    })
}

/* --- Facets ---------------------------------------------------------------- */

/// The pickers count one local day (25h across a DST change); rows are tallied
/// in memory, so a longer span is refused rather than read.
const FACETS_MAX_SPAN_MS: i64 = 48 * 60 * 60 * 1000;

async fn facets(State(state): State<AppState>, session: SessionUser, Query(params): Query<HashMap<String, String>>) -> Response {
    let (from, to) = match parse_range(&params) {
        Ok(range) => range,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    if to - from > FACETS_MAX_SPAN_MS {
        return error(StatusCode::BAD_REQUEST, "range_too_large");
    }
    let Ok(mut rows) = facet_rows_in_range(state.pool(), &session.user.id, &iso_from_ms(from), &iso_from_ms(to)).await else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    };
    let table = read_time_price_table(&state).await;
    fill_estimated_costs(&mut rows, table.as_ref());
    Json(count_facets(&rows)).into_response()
}

/// Requests and cost so far; `cost` stays `None` until a priced row joins.
#[derive(Default)]
struct Tally {
    requests: i64,
    cost: Option<f64>,
}

impl Tally {
    fn add(&mut self, cost: Option<f64>) {
        self.requests += 1;
        if let Some(cost) = cost {
            self.cost = Some(self.cost.unwrap_or(0.0) + cost);
        }
    }
}

/// Counts per provider, per `(provider, model)` and per alias, each ordered by requests desc
/// (ties keep first-seen order). Pure, so it is tested without a database.
pub fn count_facets(rows: &[FacetLogRow]) -> Value {
    let mut providers: IndexMap<&str, Tally> = IndexMap::new();
    let mut models: IndexMap<(&str, &str), Tally> = IndexMap::new();
    let mut groups: IndexMap<&str, (Tally, IndexMap<&str, i64>)> = IndexMap::new();
    let mut direct = Tally::default();
    for row in rows {
        providers.entry(&row.provider).or_default().add(row.cost);
        models.entry((&row.provider, &row.model)).or_default().add(row.cost);
        match row.group_name.as_deref() {
            Some(group) => {
                let (tally, targets) = groups.entry(group).or_default();
                tally.add(row.cost);
                *targets.entry(&row.model).or_default() += 1;
            }
            None => direct.add(row.cost),
        }
    }
    providers.sort_by(|_, a, _, b| b.requests.cmp(&a.requests));
    models.sort_by(|_, a, _, b| b.requests.cmp(&a.requests));
    groups.sort_by(|_, (a, _), _, (b, _)| b.requests.cmp(&a.requests));
    json!({
        "providers": providers.iter().map(|(provider, t)| json!({
            "provider": provider, "requests": t.requests, "cost": t.cost,
        })).collect::<Vec<_>>(),
        "models": models.iter().map(|((provider, model), t)| json!({
            "provider": provider, "model": model, "requests": t.requests, "cost": t.cost,
        })).collect::<Vec<_>>(),
        "groups": groups.iter_mut().map(|(group, (t, targets))| {
            targets.sort_by(|_, a, _, b| b.cmp(a));
            json!({ "group_name": group, "requests": t.requests, "cost": t.cost, "models": targets.keys().collect::<Vec<_>>() })
        }).collect::<Vec<_>>(),
        "direct": { "requests": direct.requests, "cost": direct.cost },
    })
}

/* --- Export ---------------------------------------------------------------- */

/// Every field an export can carry, in the default (all fields) order. The `api_keys` id is
/// not one of them.
pub const EXPORT_FIELDS: [&str; 18] = [
    "time",
    "id",
    "provider",
    "model",
    "group",
    "account",
    "account_id",
    "api_key",
    "type",
    "status",
    "upstream_status",
    "error_code",
    "input_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "output_tokens",
    "cost",
    "latency_ms",
];

/// `None` for an empty list, an unknown key, or a repeated one.
pub fn parse_columns(value: Option<&str>) -> Option<Vec<&'static str>> {
    let Some(value) = value else { return Some(EXPORT_FIELDS.to_vec()) };
    let mut columns: Vec<&'static str> = Vec::new();
    for key in value.split(',') {
        let field = EXPORT_FIELDS.iter().find(|f| **f == key)?;
        if columns.contains(field) {
            return None;
        }
        columns.push(field);
    }
    (!columns.is_empty()).then_some(columns)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportFormat {
    Csv,
    Json,
}

/// One field of one row as JSON; the CSV writer prints the same values.
fn field_value(field: &str, row: &RequestLogPageRow, names: &Resolved, offset_minutes: i64, format: ExportFormat) -> Value {
    match field {
        "time" if format == ExportFormat::Csv => Value::String(local_iso(&row.created_at, offset_minutes)),
        "time" => Value::String(row.created_at.clone()),
        "id" => json!(row.id),
        "provider" => json!(row.provider),
        "model" => json!(row.model),
        "group" => json!(row.group_name),
        "account" => json!(names.account_label),
        "account_id" => json!(row.account_id),
        "api_key" => json!(names.api_key_name),
        "type" => json!(if is_builtin_provider(&row.provider) { "oauth" } else { "api" }),
        "status" => json!(row.status_code),
        "upstream_status" => json!(row.upstream_status),
        "error_code" => json!(row.error_code),
        "input_tokens" => json!(row.prompt_tokens),
        "cache_read_tokens" => json!(row.cache_read_input_tokens),
        "cache_write_tokens" => json!(row.cache_creation_input_tokens),
        "output_tokens" => json!(row.completion_tokens),
        "cost" => json!(row.cost),
        "latency_ms" => json!(row.latency_ms),
        _ => Value::Null,
    }
}

/// `2026-11-13T14:31:52.000Z` at offset 480 → `2026-11-13T22:31:52.000+08:00`.
pub fn local_iso(created_at: &str, offset_minutes: i64) -> String {
    let Some(ms) = parse_iso_ms(created_at) else { return created_at.to_string() };
    let shifted = iso_from_ms(ms + offset_minutes * 60_000);
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let abs = offset_minutes.abs();
    format!("{}{sign}{:02}:{:02}", shifted.trim_end_matches('Z'), abs / 60, abs % 60)
}

/// RFC 4180 quoting. Text that a spreadsheet would read as a formula gets a leading `'`, so
/// an account label like `=HYPERLINK(...)` stays text when the file is opened.
pub fn csv_cell(value: &Value) -> String {
    let text = match value {
        Value::Null => return String::new(),
        Value::String(s) if s.starts_with(['=', '+', '-', '@', '\t', '\r']) => format!("'{s}"),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if text.contains([',', '"', '\n', '\r']) || text.starts_with(' ') || text.ends_with(' ') {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

/// Everything one export needs across its batches.
struct ExportJob {
    state: AppState,
    user_id: String,
    provider: Option<String>,
    model: Option<String>,
    group: Option<String>,
    direct: bool,
    errors_only: bool,
    from: String,
    to: String,
    columns: Vec<&'static str>,
    format: ExportFormat,
    offset_minutes: i64,
    names: Names,
    table: Option<PriceTable>,
    cursor: Option<(String, String)>,
    wrote_row: bool,
    done: bool,
}

impl ExportJob {
    /// The next chunk of the file, or `None` once the closing chunk is out.
    async fn next_chunk(&mut self) -> Option<Result<Bytes, std::io::Error>> {
        if self.done {
            return None;
        }
        let query = LogPageQuery {
            user_id: &self.user_id,
            provider: self.provider.as_deref(),
            model: self.model.as_deref(),
            group: self.group.as_deref(),
            direct: self.direct,
            errors_only: self.errors_only,
            from: Some(&self.from),
            to: Some(&self.to),
            cursor: self.cursor.as_ref().map(|(c, id)| (c.as_str(), id.as_str())),
            limit: EXPORT_BATCH,
        };
        let mut rows = match list_request_log_page(self.state.pool(), &query).await {
            Ok(rows) => rows,
            Err(error) => {
                // Aborts the download mid-file: a truncated file must not look complete.
                tracing::error!(%error, "logs export read failed");
                self.done = true;
                return Some(Err(std::io::Error::other("logs export read failed")));
            }
        };
        fill_estimated_costs(&mut rows, self.table.as_ref());
        let shared = self.names.shared_labels(&self.state, &self.user_id, &rows).await;
        let mut out = String::new();
        for row in &rows {
            let names = self.names.resolve(row, &shared);
            match self.format {
                ExportFormat::Csv => {
                    let cells: Vec<String> = self
                        .columns
                        .iter()
                        .map(|f| csv_cell(&field_value(f, row, &names, self.offset_minutes, self.format)))
                        .collect();
                    out.push_str(&cells.join(","));
                    out.push_str("\r\n");
                }
                ExportFormat::Json => {
                    let object: serde_json::Map<String, Value> = self
                        .columns
                        .iter()
                        .map(|f| (f.to_string(), field_value(f, row, &names, self.offset_minutes, self.format)))
                        .collect();
                    out.push_str(if self.wrote_row { ",\n" } else { "\n" });
                    out.push_str(&Value::Object(object).to_string());
                    self.wrote_row = true;
                }
            }
        }
        match rows.last() {
            Some(last) if rows.len() as i64 == EXPORT_BATCH => {
                self.cursor = Some((last.created_at.clone(), last.id.clone()));
            }
            _ => {
                self.done = true;
                if self.format == ExportFormat::Json {
                    out.push_str("\n]\n");
                }
            }
        }
        Some(Ok(Bytes::from(out)))
    }
}

async fn export(State(state): State<AppState>, session: SessionUser, Query(params): Query<HashMap<String, String>>) -> Response {
    let (from, to) = match parse_range(&params) {
        Ok(range) => range,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    let filters = match filters(&params) {
        Ok(f) => f,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    let format = match params.get("format").map(String::as_str) {
        Some("csv") => ExportFormat::Csv,
        Some("json") => ExportFormat::Json,
        _ => return error(StatusCode::BAD_REQUEST, "invalid_format"),
    };
    let Some(columns) = parse_columns(params.get("columns").map(String::as_str)) else {
        return error(StatusCode::BAD_REQUEST, "invalid_columns");
    };
    // Minutes east of UTC for the CSV's local times, bounded like the summary's offset.
    let offset_minutes = match params.get("offset") {
        None => 0,
        Some(value) => match js_number(value) {
            Some(v) if v.fract() == 0.0 && v.abs() <= 840.0 => v as i64,
            _ => return error(StatusCode::BAD_REQUEST, "invalid_offset"),
        },
    };
    let Ok(names) = Names::load(&state, &session.user.id).await else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    };
    let table = read_time_price_table(&state).await;

    let (extension, content_type, head) = match format {
        ExportFormat::Csv => ("csv", "text/csv; charset=utf-8", format!("\u{FEFF}{}\r\n", columns.join(","))),
        ExportFormat::Json => ("json", "application/json", "[".to_string()),
    };
    let local_date = |ms: i64| iso_from_ms(ms + offset_minutes * 60_000)[..10].to_string();
    let filename = format!("kano-proxy-logs-{}-{}.{extension}", local_date(from), local_date(to));

    let job = ExportJob {
        state,
        user_id: session.user.id.clone(),
        provider: filters.provider.map(str::to_string),
        model: filters.model.map(str::to_string),
        group: filters.group.map(str::to_string),
        direct: filters.direct,
        errors_only: filters.errors_only,
        from: iso_from_ms(from),
        to: iso_from_ms(to),
        columns,
        format,
        offset_minutes,
        names,
        table,
        cursor: None,
        wrote_row: false,
        done: false,
    };
    let head = futures::stream::once(async move { Ok::<Bytes, std::io::Error>(Bytes::from(head)) });
    let body = futures::stream::unfold(job, |mut job| async move { job.next_chunk().await.map(|chunk| (chunk, job)) });
    let stream = futures::StreamExt::chain(head, body);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\""))
        .body(Body::from_stream(stream))
        .expect("response builds")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session::create_session;
    use crate::db::request_logs::{insert_request_log, RequestLogEntry};
    use crate::db::test_support::{insert_user, skip_without_db, test_pool, test_state, TEST_APP_URL};
    use crate::extensions::Extensions;
    use crate::pool::extension::{ListSharedOptions, PoolExtension, ReserveContext, ReserveOutcome, SharedAccount};
    use crate::providers::ProviderId;
    use crate::routing::types::RoutingCandidate;
    use crate::upstream::{MockTransport, UpstreamResponse};
    use axum::body::Body;
    use axum::http::{header, Request};
    use http_body_util::BodyExt;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    /// A transport that answers every upstream attempt (the read-time price fetch) with a 500,
    /// so no test ever reaches the real network. Enough handlers are queued for the price
    /// refresh both sources make on every admin request in the test.
    fn offline() -> Arc<MockTransport> {
        let mock = MockTransport::new();
        for _ in 0..64 {
            mock.expect(|_| {
                Ok(UpstreamResponse::from_bytes(StatusCode::INTERNAL_SERVER_ERROR, http::HeaderMap::new(), "offline"))
            });
        }
        mock
    }

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn signed_in(state: &AppState, email: &str) -> (String, String) {
        let user = insert_user(state.pool(), email).await;
        let (_, cookie) = create_session(state, &user.id, false).await.unwrap();
        (user.id, cookie.split(';').next().unwrap().to_string())
    }

    /// Seeds one `request_logs` row, overriding `created_at` and `id` after the insert (the
    /// writer stamps both).
    async fn seed_log(state: &AppState, user_id: &str, id: Option<&str>, overrides: Value) {
        let mut base = json!({
            "user_id": user_id,
            "provider": "claude-code",
            "model": "claude-code/claude-opus-5",
            "status_code": 200,
            "latency_ms": 100,
            "started_at": crate::ids::now_iso(),
        });
        let mut created_at = "2026-08-14T12:00:00.000Z".to_string();
        for (k, v) in overrides.as_object().expect("overrides object") {
            if k == "created_at" {
                created_at = v.as_str().expect("iso string").to_string();
                continue;
            }
            base[k] = v.clone();
        }
        let entry: RequestLogEntry = serde_json::from_value(base).expect("a log entry");
        let written = insert_request_log(state.pool(), &entry).await.unwrap();
        sqlx::query("UPDATE request_logs SET created_at = $1, id = $2 WHERE id = $3")
            .bind(&created_at)
            .bind(id.unwrap_or(&written))
            .bind(&written)
            .execute(state.pool())
            .await
            .unwrap();
    }

    async fn seed_account(state: &AppState, user_id: &str, id: &str, label: Option<&str>, custom_label: Option<&str>) {
        sqlx::query(
            "INSERT INTO upstream_accounts (id, user_id, provider, external_account_id, label, custom_label, priority,
                                            encrypted_payload, created_at, updated_at)
             VALUES ($1, $2, 'claude-code', NULL, $3, $4, 1, 'encrypted', $5, $5)",
        )
        .bind(id)
        .bind(user_id)
        .bind(label)
        .bind(custom_label)
        .bind(crate::ids::now_iso())
        .execute(state.pool())
        .await
        .unwrap();
    }

    fn router(state: AppState) -> axum::Router {
        crate::build_router(state, Extensions::default())
    }

    async fn get(router: axum::Router, cookie: &str, uri: &str) -> Response {
        router
            .oneshot(Request::builder().uri(uri).header(header::COOKIE, cookie).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[test]
    fn cursors_round_trip_and_reject_junk() {
        let cursor = Cursor { created_at: "2026-08-14T12:00:00.000Z".into(), id: "log_a".into() };
        let encoded = encode_cursor(&cursor);
        assert_eq!(decode_cursor(&encoded), Some(cursor));
        for bad in ["not-a-cursor", "", &BASE64.encode("no-space"), &BASE64.encode(" leading"), &BASE64.encode("two spaces here")] {
            assert_eq!(decode_cursor(bad), None, "{bad}");
        }
        assert_eq!(decode_cursor(&BASE64.encode("2026-08-14T12:00:00.000Z ")), None, "an empty id is no cursor");
    }

    #[test]
    fn the_limit_defaults_to_fifty_and_is_bounded() {
        assert_eq!(parse_limit(None), Some(50));
        assert_eq!(parse_limit(Some("1")), Some(1));
        assert_eq!(parse_limit(Some("100")), Some(100));
        for bad in ["0", "101", "1.5", "-1", "abc", ""] {
            assert_eq!(parse_limit(Some(bad)), None, "{bad}");
        }
    }

    #[tokio::test]
    async fn requires_a_session() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let response = router(state).oneshot(Request::builder().uri("/api/logs").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await, json!({ "error": "unauthorized" }));
    }

    #[tokio::test]
    async fn uses_the_default_limit_and_rejects_invalid_limits() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "limit@example.com").await;
        for i in 0..51 {
            seed_log(
                &state,
                &user_id,
                Some(&format!("log_{i:03}")),
                json!({ "created_at": format!("2026-08-14T12:{i:02}:00.000Z") }),
            )
            .await;
        }

        let json = body_json(get(router(state.clone()), &cookie, "/api/logs").await).await;
        assert_eq!(json["rows"].as_array().unwrap().len(), 50);
        assert!(json["next_cursor"].is_string());

        for bad in ["0", "101", "1.5", "-1", "abc", ""] {
            let response = get(router(state.clone()), &cookie, &format!("/api/logs?limit={bad}")).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(body_json(response).await, json!({ "error": "invalid_limit" }));
        }
    }

    #[tokio::test]
    async fn cursor_pages_stably_by_created_at_then_id() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "cursor@example.com").await;
        for id in ["log_a", "log_c", "log_b", "log_d", "log_e"] {
            seed_log(&state, &user_id, Some(id), json!({})).await;
        }

        let first = body_json(get(router(state.clone()), &cookie, "/api/logs?limit=2").await).await;
        assert_eq!(ids(&first), ["log_e", "log_d"]);
        let cursor = first["next_cursor"].as_str().expect("a next cursor").to_string();

        let second =
            body_json(get(router(state.clone()), &cookie, &format!("/api/logs?limit=2&cursor={}", urlencode(&cursor))).await).await;
        assert_eq!(ids(&second), ["log_c", "log_b"]);
        let cursor = second["next_cursor"].as_str().expect("a next cursor").to_string();

        let third =
            body_json(get(router(state.clone()), &cookie, &format!("/api/logs?limit=2&cursor={}", urlencode(&cursor))).await).await;
        assert_eq!(ids(&third), ["log_a"]);
        assert_eq!(third["next_cursor"], Value::Null, "the last page carries no cursor");

        let response = get(router(state), &cookie, "/api/logs?cursor=not-a-cursor").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await, json!({ "error": "invalid_cursor" }));
    }

    fn ids(json: &Value) -> Vec<String> {
        json["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
    }

    fn urlencode(value: &str) -> String {
        value.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D")
    }

    #[tokio::test]
    async fn filters_by_exact_provider_and_by_errors() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "filter@example.com").await;
        seed_log(&state, &user_id, Some("builtin"), json!({ "provider": "claude-code" })).await;
        seed_log(&state, &user_id, Some("custom"), json!({ "provider": "deleted-custom" })).await;
        seed_log(&state, &user_id, Some("error-code"), json!({ "provider": "deleted-custom", "error_code": "invalid_model" })).await;
        seed_log(&state, &user_id, Some("error-status"), json!({ "provider": "deleted-custom", "status_code": 503 })).await;

        let json = body_json(get(router(state.clone()), &cookie, "/api/logs?provider=deleted-custom").await).await;
        let mut got = ids(&json);
        got.sort();
        assert_eq!(got, ["custom", "error-code", "error-status"], "a deleted provider is still filterable");

        let json = body_json(get(router(state), &cookie, "/api/logs?errors=1").await).await;
        let mut got = ids(&json);
        got.sort();
        assert_eq!(got, ["error-code", "error-status"]);
    }

    #[tokio::test]
    async fn resolves_display_fields_derives_usage_type_and_prices_null_costs() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let mock = MockTransport::new();
        for _ in 0..8 {
            mock.expect(|request| {
                if request.url.contains("openrouter.ai") {
                    Ok(UpstreamResponse::json(StatusCode::OK, &json!({ "data": [] })))
                } else {
                    Ok(UpstreamResponse::json(
                        StatusCode::OK,
                        &json!({ "claude-opus-5": { "input_cost_per_token": 0.00001, "output_cost_per_token": 0.00005 } }),
                    ))
                }
            });
        }
        let state = test_state(pool, mock);
        let (user_id, cookie) = signed_in(&state, "display@example.com").await;
        let other = insert_user(state.pool(), "other@example.com").await;
        seed_account(&state, &user_id, "account_live", Some("Upstream name"), Some("Primary")).await;
        seed_account(&state, &user_id, "account_fallback", Some("Fallback name"), None).await;
        seed_account(&state, &other.id, "other_account", Some("Leaked"), Some("Leaked")).await;
        let key = crate::db::keys::create_key(state.pool(), &user_id, "Production", None).await.unwrap().row;
        crate::db::keys::create_key(state.pool(), &other.id, "Leaked", None).await.unwrap();

        seed_log(
            &state,
            &user_id,
            Some("oauth"),
            json!({ "account_id": "account_live", "api_key_id": key.id, "prompt_tokens": 100, "completion_tokens": 10 }),
        )
        .await;
        seed_log(
            &state,
            &user_id,
            Some("custom"),
            json!({ "provider": "deleted-custom", "model": "deleted-custom/model", "account_id": "account_fallback",
                    "api_key_id": "deleted_key" }),
        )
        .await;
        seed_log(&state, &user_id, Some("deleted-account"), json!({ "account_id": "deleted_account" })).await;
        // Written after the name-snapshot migration: both records being gone still leaves
        // something to read.
        seed_log(
            &state,
            &user_id,
            Some("named-and-gone"),
            json!({ "account_id": "deleted_account_2", "account_label": "Old laptop", "api_key_id": "deleted_key_2",
                    "api_key_name": "Old CI key" }),
        )
        .await;

        let json = body_json(get(router(state), &cookie, "/api/logs?limit=10").await).await;
        let rows = json["rows"].as_array().unwrap();
        for row in rows {
            assert!(row.get("api_key_id").is_none(), "the api_keys id never leaves the process");
        }
        let by_id = |id: &str| rows.iter().find(|r| r["id"] == id).expect("row").clone();

        let oauth = by_id("oauth");
        assert_eq!(oauth["account_label"], "Primary", "the user's own name wins");
        assert_eq!(oauth["account_removed"], false);
        assert_eq!(oauth["account_shared_by"], Value::Null);
        assert_eq!(oauth["api_key_name"], "Production");
        assert_eq!(oauth["api_key_removed"], false);
        assert_eq!(oauth["usage_type"], "oauth");
        let expected = 100.0 * 0.00001 + 10.0 * 0.00005;
        assert!((oauth["cost"].as_f64().unwrap() - expected).abs() < 1e-12);

        // "custom" points at a key that never appears in api_keys for this user — that is
        // exactly what api_key_removed means.
        let custom = by_id("custom");
        assert_eq!(custom["account_label"], "Fallback name");
        assert_eq!(custom["api_key_name"], Value::Null);
        assert_eq!(custom["api_key_removed"], true);
        assert_eq!(custom["usage_type"], "api");
        assert_eq!(custom["cost"], Value::Null);

        // A NULL api_key_id (no key attributed) is "not reported", not "removed".
        let deleted = by_id("deleted-account");
        assert_eq!(deleted["account_label"], Value::Null);
        assert_eq!(deleted["account_removed"], true);
        assert_eq!(deleted["api_key_name"], Value::Null);
        assert_eq!(deleted["api_key_removed"], false);

        // A removed record keeps its last name: removal is the flag, not a null name.
        let named = by_id("named-and-gone");
        assert_eq!(named["account_label"], "Old laptop");
        assert_eq!(named["account_removed"], true);
        assert_eq!(named["api_key_name"], "Old CI key");
        assert_eq!(named["api_key_removed"], true);
    }

    /// A pool extension that names exactly one borrowed account and records what it was asked.
    struct LabelStub {
        asked: Mutex<Vec<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl PoolExtension for LabelStub {
        async fn list_shared(
            &self,
            _cx: &AppState,
            _viewer_user_id: &str,
            _provider: ProviderId,
            _options: ListSharedOptions,
        ) -> anyhow::Result<Vec<SharedAccount>> {
            Ok(Vec::new())
        }
        async fn reserve_attempt(
            &self,
            _cx: &AppState,
            _ctx: ReserveContext<'_>,
            _candidate: &RoutingCandidate,
        ) -> anyhow::Result<ReserveOutcome> {
            Ok(ReserveOutcome::Ungoverned)
        }
        async fn set_shared_priority(&self, _cx: &AppState, _viewer: &str, _account_id: &str, _priority: i32) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn label_shared(
            &self,
            _cx: &AppState,
            viewer_user_id: &str,
            account_ids: &[String],
        ) -> anyhow::Result<HashMap<String, SharedLabel>> {
            let mut asked = vec![viewer_user_id.to_string()];
            asked.extend(account_ids.iter().cloned());
            self.asked.lock().unwrap().push(asked);
            Ok(HashMap::from([(
                "lent".to_string(),
                SharedLabel { label: "Team Claude".into(), owner_label: "Owner Person".into() },
            )]))
        }
    }

    #[tokio::test]
    async fn names_a_borrowed_account_through_the_pool_extension_and_never_another_users_row() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let base = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&base, "borrow@example.com").await;
        let lender = insert_user(base.pool(), "lender@example.com").await;
        seed_account(&base, &user_id, "own", Some("Mine"), None).await;
        seed_account(&base, &lender.id, "lent", Some("owner@example.test"), Some("Team Claude")).await;
        seed_account(&base, &lender.id, "withdrawn", Some("No longer shared"), None).await;
        seed_log(&base, &user_id, Some("own"), json!({ "account_id": "own" })).await;
        seed_log(&base, &user_id, Some("lent"), json!({ "account_id": "lent", "account_label": "Team Claude" })).await;
        seed_log(&base, &user_id, Some("withdrawn"), json!({ "account_id": "withdrawn", "account_label": "Was shared" })).await;

        let stub = Arc::new(LabelStub { asked: Mutex::new(Vec::new()) });
        let with_ext = AppState::builder(crate::db::test_support::test_config(), base.pool().clone())
            .transport(offline())
            .pool_extension(Some(stub.clone()))
            .build();
        let json = body_json(get(router(with_ext), &cookie, "/api/logs?limit=10").await).await;
        // Only the ids that are not the viewer's own rows are asked about, once, in page order.
        assert_eq!(
            *stub.asked.lock().unwrap(),
            vec![vec![user_id.clone(), "withdrawn".to_string(), "lent".to_string()]]
        );
        let rows = json["rows"].as_array().unwrap();
        let by_id = |id: &str| rows.iter().find(|r| r["id"] == id).expect("row").clone();
        assert_eq!(by_id("own")["account_label"], "Mine");
        assert_eq!(by_id("own")["account_removed"], false);
        assert_eq!(by_id("own")["account_shared_by"], Value::Null);
        assert_eq!(by_id("lent")["account_label"], "Team Claude");
        assert_eq!(by_id("lent")["account_removed"], false);
        assert_eq!(by_id("lent")["account_shared_by"], "Owner Person");
        // The extension did not name it, so the lender's live label must not leak: the row
        // falls back to the label stored when it was written, flagged as removed.
        assert_eq!(by_id("withdrawn")["account_label"], "Was shared");
        assert_eq!(by_id("withdrawn")["account_removed"], true);
        assert_eq!(by_id("withdrawn")["account_shared_by"], Value::Null);

        // Without an extension the same rows read as removed, by their stored names.
        let json = body_json(get(router(base), &cookie, "/api/logs?limit=10").await).await;
        let rows = json["rows"].as_array().unwrap();
        let lent = rows.iter().find(|r| r["id"] == "lent").expect("row");
        assert_eq!(lent["account_label"], "Team Claude");
        assert_eq!(lent["account_removed"], true);
        assert_eq!(lent["account_shared_by"], Value::Null);
    }

    #[tokio::test]
    async fn filters_by_model_by_alias_and_by_no_alias() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "facet-filter@example.com").await;
        seed_log(&state, &user_id, Some("opus-main"), json!({ "model": "claude-code/claude-opus-5", "group_name": "lincy/main" })).await;
        seed_log(&state, &user_id, Some("opus-direct"), json!({ "model": "claude-code/claude-opus-5" })).await;
        seed_log(&state, &user_id, Some("haiku-fast"), json!({ "model": "claude-code/claude-haiku-5", "group_name": "lincy/fast" })).await;

        let sorted = |json: Value| {
            let mut got = ids(&json);
            got.sort();
            got
        };
        let json = body_json(get(router(state.clone()), &cookie, "/api/logs?model=claude-code%2Fclaude-opus-5").await).await;
        assert_eq!(sorted(json), ["opus-direct", "opus-main"]);
        let json = body_json(get(router(state.clone()), &cookie, "/api/logs?group=lincy%2Ffast").await).await;
        assert_eq!(sorted(json), ["haiku-fast"]);
        let json = body_json(get(router(state.clone()), &cookie, "/api/logs?direct=1").await).await;
        assert_eq!(sorted(json), ["opus-direct"]);
        let json = body_json(get(router(state.clone()), &cookie, "/api/logs?model=claude-code%2Fclaude-opus-5&direct=1").await).await;
        assert_eq!(sorted(json), ["opus-direct"], "filters combine");

        let response = get(router(state), &cookie, "/api/logs?group=lincy%2Ffast&direct=1").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await, json!({ "error": "invalid_filter" }));
    }

    fn facet_row(provider: &str, model: &str, group: Option<&str>, cost: Option<f64>) -> FacetLogRow {
        FacetLogRow {
            provider: provider.into(),
            model: model.into(),
            group_name: group.map(str::to_string),
            prompt_tokens: None,
            completion_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            cost,
        }
    }

    #[test]
    fn facets_count_by_provider_model_and_alias_most_used_first() {
        let rows = [
            facet_row("codex", "codex/gpt-5", None, Some(0.5)),
            facet_row("claude-code", "claude-code/opus", Some("lincy/main"), Some(1.0)),
            facet_row("claude-code", "claude-code/opus", Some("lincy/main"), None),
            facet_row("claude-code", "claude-code/haiku", Some("lincy/main"), Some(0.25)),
            facet_row("claude-code", "claude-code/haiku", None, None),
        ];
        let facets = count_facets(&rows);
        assert_eq!(
            facets["providers"],
            json!([{ "provider": "claude-code", "requests": 4, "cost": 1.25 }, { "provider": "codex", "requests": 1, "cost": 0.5 }])
        );
        assert_eq!(
            facets["models"],
            json!([
                { "provider": "claude-code", "model": "claude-code/opus", "requests": 2, "cost": 1.0 },
                { "provider": "claude-code", "model": "claude-code/haiku", "requests": 2, "cost": 0.25 },
                { "provider": "codex", "model": "codex/gpt-5", "requests": 1, "cost": 0.5 },
            ]),
            "ties keep first-seen order"
        );
        assert_eq!(
            facets["groups"],
            json!([{ "group_name": "lincy/main", "requests": 3, "cost": 1.25, "models": ["claude-code/opus", "claude-code/haiku"] }])
        );
        // Unpriced rows count as requests but leave the cost unknown rather than zero.
        assert_eq!(facets["direct"], json!({ "requests": 2, "cost": 0.5 }));
        let only_unpriced = count_facets(&[facet_row("x", "x/y", None, None)]);
        assert_eq!(only_unpriced["direct"], json!({ "requests": 1, "cost": null }));
    }

    #[tokio::test]
    async fn facets_count_only_the_viewers_rows_inside_the_range() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "facets@example.com").await;
        let other = insert_user(state.pool(), "facets-other@example.com").await;
        seed_log(&state, &user_id, None, json!({ "created_at": "2026-08-14T10:00:00.000Z", "group_name": "g/a" })).await;
        seed_log(&state, &user_id, None, json!({ "created_at": "2026-08-14T11:00:00.000Z" })).await;
        seed_log(&state, &user_id, None, json!({ "created_at": "2026-08-13T23:59:59.000Z" })).await;
        seed_log(&state, &other.id, None, json!({ "created_at": "2026-08-14T10:30:00.000Z" })).await;

        let uri = "/api/logs/facets?from=2026-08-14T00:00:00.000Z&to=2026-08-14T23:59:59.999Z";
        let json = body_json(get(router(state.clone()), &cookie, uri).await).await;
        assert_eq!(json["providers"], json!([{ "provider": "claude-code", "requests": 2, "cost": null }]));
        assert_eq!(json["groups"][0]["group_name"], "g/a");
        assert_eq!(json["direct"]["requests"], 1);

        for bad in ["/api/logs/facets", "/api/logs/facets?from=2026-08-15T00:00:00Z&to=2026-08-14T00:00:00Z", "/api/logs/facets?from=x&to=y"] {
            let response = get(router(state.clone()), &cookie, bad).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(body_json(response).await, json!({ "error": "invalid_range" }));
        }
        let week = "/api/logs/facets?from=2026-08-07T00:00:00Z&to=2026-08-14T00:00:00Z";
        let response = get(router(state.clone()), &cookie, week).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await, json!({ "error": "range_too_large" }));
    }

    #[test]
    fn export_cells_quote_like_rfc_4180_and_defuse_formulas() {
        assert_eq!(csv_cell(&json!("plain")), "plain");
        assert_eq!(csv_cell(&json!("a,b")), "\"a,b\"");
        assert_eq!(csv_cell(&json!("say \"hi\"")), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_cell(&json!("=HYPERLINK(\"x\")")), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(csv_cell(&json!("-rm")), "'-rm");
        assert_eq!(csv_cell(&json!(-1)), "-1", "numbers are not text");
        assert_eq!(csv_cell(&Value::Null), "");
    }

    #[test]
    fn export_times_shift_into_the_callers_zone() {
        assert_eq!(local_iso("2026-11-13T14:31:52.000Z", 480), "2026-11-13T22:31:52.000+08:00");
        assert_eq!(local_iso("2026-11-13T01:00:00.000Z", -330), "2026-11-12T19:30:00.000-05:30");
        assert_eq!(local_iso("2026-11-13T01:00:00.000Z", 0), "2026-11-13T01:00:00.000+00:00");
    }

    #[test]
    fn export_columns_are_known_unique_and_default_to_all() {
        assert_eq!(parse_columns(None).unwrap(), EXPORT_FIELDS.to_vec());
        assert_eq!(parse_columns(Some("model,time")).unwrap(), ["model", "time"]);
        for bad in ["", "model,", "api_key_id", "time,time"] {
            assert_eq!(parse_columns(Some(bad)), None, "{bad}");
        }
    }

    async fn body_text(response: Response) -> String {
        String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn exports_csv_with_names_filters_local_times_and_no_key_id() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "export@example.com").await;
        seed_account(&state, &user_id, "account_live", Some("Upstream"), Some("=Primary")).await;
        let key = crate::db::keys::create_key(state.pool(), &user_id, "Lincy, main", None).await.unwrap().row;
        seed_log(
            &state,
            &user_id,
            Some("ok"),
            json!({ "created_at": "2026-08-14T12:00:00.000Z", "account_id": "account_live", "api_key_id": key.id,
                    "prompt_tokens": 10, "completion_tokens": 2, "cost": 0.5, "group_name": "lincy/main" }),
        )
        .await;
        seed_log(&state, &user_id, Some("failed"), json!({ "created_at": "2026-08-14T13:00:00.000Z", "error_code": "upstream_error" })).await;
        seed_log(&state, &user_id, Some("outside"), json!({ "created_at": "2026-08-16T00:00:00.000Z" })).await;

        let uri = "/api/logs/export?format=csv&from=2026-08-14T00:00:00.000Z&to=2026-08-15T00:00:00.000Z&offset=480\
                   &columns=time,id,group,account,api_key,status,error_code,cost";
        let response = get(router(state.clone()), &cookie, uri).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::CONTENT_TYPE).unwrap(), "text/csv; charset=utf-8");
        assert_eq!(
            response.headers().get(header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"kano-proxy-logs-2026-08-14-2026-08-15.csv\""
        );
        assert_eq!(response.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let text = body_text(response).await;
        assert!(!text.contains(&key.id), "the api_keys id never leaves the process");
        assert_eq!(
            text,
            "\u{FEFF}time,id,group,account,api_key,status,error_code,cost\r\n\
             2026-08-14T21:00:00.000+08:00,failed,,,,200,upstream_error,\r\n\
             2026-08-14T20:00:00.000+08:00,ok,lincy/main,'=Primary,\"Lincy, main\",200,,0.5\r\n"
        );

        let uri = "/api/logs/export?format=csv&from=2026-08-14T00:00:00.000Z&to=2026-08-15T00:00:00.000Z&errors=1&columns=id";
        assert_eq!(body_text(get(router(state.clone()), &cookie, uri).await).await, "\u{FEFF}id\r\nfailed\r\n");

        for (bad, code) in [
            ("/api/logs/export?format=xml&from=2026-08-14T00:00:00Z&to=2026-08-15T00:00:00Z", "invalid_format"),
            ("/api/logs/export?format=csv&from=2026-08-14T00:00:00Z&to=2026-08-15T00:00:00Z&columns=nope", "invalid_columns"),
            ("/api/logs/export?format=csv&from=2026-08-14T00:00:00Z&to=2026-08-15T00:00:00Z&offset=900", "invalid_offset"),
            ("/api/logs/export?format=csv&to=2026-08-15T00:00:00Z", "invalid_range"),
            ("/api/logs/export?format=csv&from=2020-01-01T00:00:00Z&to=2026-08-15T00:00:00Z", "range_too_large"),
            ("/api/logs/export?format=csv&from=2026-08-14T00:00:00Z&to=2026-08-15T00:00:00Z&group=a&direct=1", "invalid_filter"),
        ] {
            let response = get(router(state.clone()), &cookie, bad).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(body_json(response).await, json!({ "error": code }));
        }
    }

    #[tokio::test]
    async fn exports_json_across_batches_as_one_array() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (user_id, cookie) = signed_in(&state, "export-json@example.com").await;
        let total = EXPORT_BATCH as usize + 1;
        for i in 0..total {
            seed_log(&state, &user_id, Some(&format!("log_{i:04}")), json!({ "created_at": "2026-08-14T12:00:00.000Z" })).await;
        }
        let uri = "/api/logs/export?format=json&from=2026-08-14T00:00:00.000Z&to=2026-08-15T00:00:00.000Z&columns=id,time,cost";
        let json: Value = serde_json::from_str(&body_text(get(router(state.clone()), &cookie, uri).await).await).unwrap();
        let rows = json.as_array().unwrap();
        assert_eq!(rows.len(), total, "the second batch continues after the cursor, with nothing repeated");
        assert_eq!(rows[0], json!({ "id": format!("log_{:04}", total - 1), "time": "2026-08-14T12:00:00.000Z", "cost": null }));
        assert_eq!(rows[total - 1]["id"], "log_0000");

        let empty = "/api/logs/export?format=json&from=2026-01-01T00:00:00.000Z&to=2026-01-02T00:00:00.000Z";
        let json: Value = serde_json::from_str(&body_text(get(router(state), &cookie, empty).await).await).unwrap();
        assert_eq!(json, json!([]));
    }

    #[tokio::test]
    async fn the_admin_cors_and_no_store_rules_apply() {
        let Some(pool) = test_pool().await else { return skip_without_db() };
        let state = test_state(pool, offline());
        let (_, cookie) = signed_in(&state, "cors@example.com").await;
        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/logs")
                    .header(header::COOKIE, &cookie)
                    .header(header::ORIGIN, TEST_APP_URL)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(), TEST_APP_URL);
        assert_eq!(response.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
    }
}
