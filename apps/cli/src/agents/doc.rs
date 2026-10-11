//! The two config documents `kano-proxy agent` edits, behind one field
//! interface: Claude Code's `settings.json` (JSON, key order kept) and the
//! Codex CLI's `config.toml` (`toml_edit`, so comments and everything the
//! user wrote survive). Fields are dotted paths — `env.ANTHROPIC_MODEL`,
//! `model_providers.kano` — and values cross the boundary as
//! `serde_json::Value`, which is also what the state file records for
//! restore and drift (docs/cli.md § Command surface, § State file).

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Json,
    Toml,
}

pub enum Doc {
    Json(Value),
    Toml(DocumentMut),
}

impl Doc {
    /// A missing file is an empty document — the agent may never have been run.
    pub fn load(kind: Kind, path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        Self::parse(kind, &text).with_context(|| format!("{} is not valid {}", path.display(), kind.name()))
    }

    pub fn parse(kind: Kind, text: &str) -> Result<Self> {
        Ok(match kind {
            Kind::Json => {
                if text.trim().is_empty() {
                    Doc::Json(Value::Object(Map::new()))
                } else {
                    let value: Value = serde_json::from_str(text)?;
                    if !value.is_object() {
                        bail!("the top level is not an object");
                    }
                    Doc::Json(value)
                }
            }
            Kind::Toml => Doc::Toml(text.parse::<DocumentMut>()?),
        })
    }

    pub fn get(&self, field: &str) -> Option<Value> {
        let parts: Vec<&str> = field.split('.').collect();
        match self {
            Doc::Json(root) => {
                let mut cur = root;
                for part in &parts {
                    cur = cur.as_object()?.get(*part)?;
                }
                Some(cur.clone())
            }
            Doc::Toml(doc) => {
                let mut cur: &Item = doc.as_item();
                for part in &parts {
                    cur = cur.as_table_like()?.get(part)?;
                }
                Some(toml_to_json(cur))
            }
        }
    }

    pub fn set(&mut self, field: &str, value: &Value) {
        let parts: Vec<&str> = field.split('.').collect();
        let (last, prefix) = parts.split_last().expect("a field has at least one segment");
        match self {
            Doc::Json(root) => {
                let mut cur = root;
                for part in prefix {
                    let obj = cur.as_object_mut().expect("the top level is an object");
                    let next = obj.entry(part.to_string()).or_insert_with(|| Value::Object(Map::new()));
                    if !next.is_object() {
                        *next = Value::Object(Map::new());
                    }
                    cur = next;
                }
                cur.as_object_mut().expect("an object").insert(last.to_string(), value.clone());
            }
            Doc::Toml(doc) => {
                let mut cur: &mut Item = doc.as_item_mut();
                for part in prefix {
                    let table = cur.as_table_mut().expect("a table");
                    if !table.contains_table(part) {
                        table.insert(part, Item::Table(Table::new()));
                    }
                    cur = table.get_mut(part).expect("just inserted");
                }
                let table = cur.as_table_mut().expect("a table");
                let new_item = json_to_toml(value);
                // An existing key is updated in place so the user's decor survives: the
                // comment above the key (on the key) and the one after the value (on the
                // value) — `Table::insert` would replace both with a bare key.
                match (table.get_mut(last), new_item) {
                    (Some(Item::Value(existing)), Item::Value(mut fresh)) => {
                        *fresh.decor_mut() = existing.decor().clone();
                        *existing = fresh;
                    }
                    (Some(existing), fresh) => *existing = fresh,
                    (None, fresh) => {
                        table.insert(last, fresh);
                    }
                }
            }
        }
    }

    pub fn remove(&mut self, field: &str) {
        let parts: Vec<&str> = field.split('.').collect();
        let (last, prefix) = parts.split_last().expect("a field has at least one segment");
        match self {
            Doc::Json(root) => {
                let mut cur = root;
                for part in prefix {
                    let Some(next) = cur.as_object_mut().and_then(|o| o.get_mut(*part)) else { return };
                    cur = next;
                }
                if let Some(obj) = cur.as_object_mut() {
                    obj.remove(*last);
                }
            }
            Doc::Toml(doc) => {
                let mut cur: &mut Item = doc.as_item_mut();
                for part in prefix {
                    let Some(next) = cur.as_table_mut().and_then(|t| t.get_mut(part)) else { return };
                    cur = next;
                }
                if let Some(table) = cur.as_table_mut() {
                    table.remove(last);
                }
            }
        }
    }

    pub fn render(&self) -> String {
        match self {
            // Two-space pretty JSON with a trailing newline — what Claude Code itself writes.
            Doc::Json(value) => format!("{}\n", serde_json::to_string_pretty(value).expect("json serializes")),
            Doc::Toml(doc) => doc.to_string(),
        }
    }

    /// Atomic replace, mode 0600 — the file holds the API key.
    pub fn write(&self, path: &Path) -> Result<()> {
        write_private(path, self.render().as_bytes())
    }
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Json => "JSON",
            Kind::Toml => "TOML",
        }
    }
}

/// Write-temp-then-rename with 0600 permissions, creating the parent directory.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("config path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = path.with_extension(format!(
        "{}.kano-tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("cannot write {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn toml_to_json(item: &Item) -> Value {
    match item {
        Item::None => Value::Null,
        Item::Value(v) => toml_value_to_json(v),
        Item::Table(t) => Value::Object(t.iter().map(|(k, v)| (k.to_string(), toml_to_json(v))).collect()),
        Item::ArrayOfTables(a) => {
            Value::Array(a.iter().map(|t| Value::Object(t.iter().map(|(k, v)| (k.to_string(), toml_to_json(v))).collect())).collect())
        }
    }
}

fn toml_value_to_json(v: &toml_edit::Value) -> Value {
    use toml_edit::Value as T;
    match v {
        T::String(s) => Value::String(s.value().clone()),
        T::Integer(i) => Value::from(*i.value()),
        T::Float(f) => serde_json::Number::from_f64(*f.value()).map(Value::Number).unwrap_or(Value::Null),
        T::Boolean(b) => Value::Bool(*b.value()),
        T::Datetime(d) => Value::String(d.to_string()),
        T::Array(a) => Value::Array(a.iter().map(toml_value_to_json).collect()),
        T::InlineTable(t) => Value::Object(t.iter().map(|(k, v)| (k.to_string(), toml_value_to_json(v))).collect()),
    }
}

/// A JSON object becomes a regular (not inline) table, so `[model_providers.kano]` reads the
/// way the docs page writes it; everything else a value.
fn json_to_toml(value: &Value) -> Item {
    match value {
        Value::Object(map) => {
            let mut table = Table::new();
            for (k, v) in map {
                table.insert(k, json_to_toml(v));
            }
            Item::Table(table)
        }
        other => Item::Value(json_to_toml_value(other)),
    }
}

fn json_to_toml_value(value: &Value) -> toml_edit::Value {
    match value {
        Value::Null => toml_edit::Value::from(""),
        Value::Bool(b) => toml_edit::Value::from(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml_edit::Value::from(i)
            } else {
                toml_edit::Value::from(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => toml_edit::Value::from(s.as_str()),
        Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(json_to_toml_value(item));
            }
            toml_edit::Value::Array(array)
        }
        Value::Object(map) => {
            let mut table = toml_edit::InlineTable::new();
            for (k, v) in map {
                table.insert(k, json_to_toml_value(v));
            }
            toml_edit::Value::InlineTable(table)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_fields_round_trip_and_keep_the_rest() {
        let mut doc = Doc::parse(Kind::Json, r#"{"permissions":{"allow":["Bash"]},"env":{"FOO":"1"},"model":"opus"}"#).unwrap();
        assert_eq!(doc.get("env.FOO"), Some(json!("1")));
        assert_eq!(doc.get("env.MISSING"), None);
        doc.set("env.ANTHROPIC_BASE_URL", &json!("https://x/g/claude-code/anthropic"));
        doc.set("env.FOO", &json!("2"));
        doc.remove("env.FOO");
        let out = doc.render();
        let back: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(back["permissions"]["allow"][0], "Bash");
        assert_eq!(back["model"], "opus");
        assert_eq!(back["env"]["ANTHROPIC_BASE_URL"], "https://x/g/claude-code/anthropic");
        assert!(back["env"].get("FOO").is_none());
        // Key order is the file's own, not alphabetical.
        assert!(out.find("permissions").unwrap() < out.find("env").unwrap());
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn empty_and_missing_json_is_an_object() {
        let doc = Doc::parse(Kind::Json, "").unwrap();
        assert_eq!(doc.render(), "{}\n");
        assert!(Doc::parse(Kind::Json, "[]").is_err());
        assert!(Doc::load(Kind::Json, Path::new("/nonexistent/kano/settings.json")).is_ok());
    }

    #[test]
    fn toml_fields_keep_comments_and_write_a_real_table() {
        let src = "# my codex config\nmodel = \"o3\" # old\nsandbox_mode = \"workspace-write\"\n\n[model_providers.other]\nname = \"Other\"\n";
        let mut doc = Doc::parse(Kind::Toml, src).unwrap();
        assert_eq!(doc.get("model"), Some(json!("o3")));
        assert_eq!(doc.get("model_providers.other.name"), Some(json!("Other")));
        assert_eq!(doc.get("model_providers.kano"), None);
        doc.set("model", &json!("gpt-5.6-sol"));
        doc.set("model_provider", &json!("kano"));
        doc.set(
            "model_providers.kano",
            &json!({ "name": "Kano Proxy", "base_url": "https://x/g/codex-cli/openai/v1", "experimental_bearer_token": "sk", "wire_api": "responses" }),
        );
        let out = doc.render();
        assert!(out.contains("# my codex config"), "{out}");
        assert!(out.contains("# old"), "{out}");
        assert!(out.contains("sandbox_mode = \"workspace-write\""));
        assert!(out.contains("[model_providers.other]"));
        assert!(out.contains("[model_providers.kano]"), "{out}");
        assert!(out.contains("wire_api = \"responses\""));
        assert!(out.contains("model_provider = \"kano\""));
        assert_eq!(doc.get("model_providers.kano.name"), Some(json!("Kano Proxy")));
        doc.remove("model_providers.kano");
        assert_eq!(doc.get("model_providers.kano"), None);
        assert!(doc.render().contains("[model_providers.other]"));
        doc.remove("model_provider");
        assert_eq!(doc.get("model_provider"), None);
    }

    #[test]
    fn toml_top_level_keys_stay_above_tables() {
        // A key set after a table exists must still land at the top level, not inside
        // the table — the docs page warns about exactly this mistake.
        let mut doc = Doc::parse(Kind::Toml, "[model_providers.kano]\nname = \"Kano Proxy\"\n").unwrap();
        doc.set("model_catalog_json", &json!("kano-models.json"));
        let out = doc.render();
        let top = out.find("model_catalog_json").unwrap();
        let table = out.find("[model_providers.kano]").unwrap();
        assert!(top < table, "{out}");
        let reparsed = Doc::parse(Kind::Toml, &out).unwrap();
        assert_eq!(reparsed.get("model_catalog_json"), Some(json!("kano-models.json")));
        assert_eq!(reparsed.get("model_providers.kano.model_catalog_json"), None);
    }
}
