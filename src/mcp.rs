//! MCP servers, and how each agent's config file spells them.
//!
//! Reads and writes touch only the servers named; every other key in the
//! file, and the order of keys, stays as it was.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::agents::{McpFile, McpFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Stdio,
    Http,
    Sse,
}

impl Transport {
    pub fn label(self) -> &'static str {
        match self {
            Transport::Stdio => "stdio",
            Transport::Http => "http",
            Transport::Sse => "sse",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Server {
    pub name: String,
    pub transport: Transport,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

impl Server {
    pub fn remote(&self) -> bool {
        self.transport != Transport::Stdio
    }

    /// One line saying what the server runs or reaches.
    pub fn summary(&self) -> String {
        if self.remote() {
            self.url.clone()
        } else {
            std::iter::once(self.command.as_str())
                .chain(self.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        }
    }

    pub fn validate(&self) -> Result<()> {
        valid_name(&self.name)?;
        if self.remote() {
            if !(self.url.starts_with("http://") || self.url.starts_with("https://")) {
                bail!("the URL has to start with http:// or https://");
            }
        } else if self.command.trim().is_empty() {
            bail!("a command is needed");
        }
        Ok(())
    }
}

/// A name is a TOML bare key and a plain file name.
pub fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        bail!("name {name:?}: use letters, digits, - and _ only")
    }
}

/// Why an agent can't reach a server, or None when it can.
pub fn unsupported(format: McpFormat, s: &Server) -> Option<&'static str> {
    match (format, s.transport) {
        (McpFormat::Desktop, Transport::Http | Transport::Sse) => {
            Some("remote servers go in Connectors")
        }
        (McpFormat::Codex, Transport::Sse) => Some("no SSE support"),
        _ => None,
    }
}

fn container_key(format: McpFormat) -> &'static str {
    match format {
        McpFormat::OpenCode => "mcp",
        McpFormat::Codex => "mcp_servers",
        _ => "mcpServers",
    }
}

fn str_map(m: &BTreeMap<String, String>) -> Value {
    Value::Object(
        m.iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    )
}

/// The entry an agent's file holds for a server, keys in the agent's order.
pub fn encode(format: McpFormat, s: &Server) -> Value {
    let mut o = Map::new();
    let opt = |o: &mut Map<String, Value>, k: &str, m: &BTreeMap<String, String>| {
        if !m.is_empty() {
            o.insert(k.into(), str_map(m));
        }
    };
    match format {
        McpFormat::Claude => {
            o.insert("type".into(), json!(s.transport.label()));
            if s.remote() {
                o.insert("url".into(), json!(s.url));
                opt(&mut o, "headers", &s.headers);
            } else {
                o.insert("command".into(), json!(s.command));
                o.insert("args".into(), json!(s.args));
                o.insert("env".into(), str_map(&s.env));
            }
        }
        McpFormat::Gemini => match s.transport {
            Transport::Http => {
                o.insert("httpUrl".into(), json!(s.url));
                opt(&mut o, "headers", &s.headers);
            }
            Transport::Sse => {
                o.insert("url".into(), json!(s.url));
                opt(&mut o, "headers", &s.headers);
            }
            Transport::Stdio => {
                o.insert("command".into(), json!(s.command));
                o.insert("args".into(), json!(s.args));
                opt(&mut o, "env", &s.env);
            }
        },
        McpFormat::OpenCode => {
            if s.remote() {
                o.insert("type".into(), json!("remote"));
                o.insert("url".into(), json!(s.url));
                opt(&mut o, "headers", &s.headers);
            } else {
                o.insert("type".into(), json!("local"));
                let mut cmd = vec![s.command.clone()];
                cmd.extend(s.args.iter().cloned());
                o.insert("command".into(), json!(cmd));
                opt(&mut o, "environment", &s.env);
            }
            o.insert("enabled".into(), json!(true));
        }
        McpFormat::Cursor | McpFormat::Desktop => {
            if s.remote() {
                o.insert("url".into(), json!(s.url));
                opt(&mut o, "headers", &s.headers);
            } else {
                o.insert("command".into(), json!(s.command));
                o.insert("args".into(), json!(s.args));
                opt(&mut o, "env", &s.env);
            }
        }
        McpFormat::Copilot => {
            if s.remote() {
                o.insert("type".into(), json!(s.transport.label()));
                o.insert("url".into(), json!(s.url));
                opt(&mut o, "headers", &s.headers);
            } else {
                o.insert("type".into(), json!("local"));
                o.insert("command".into(), json!(s.command));
                o.insert("args".into(), json!(s.args));
                opt(&mut o, "env", &s.env);
            }
            o.insert("tools".into(), json!(["*"]));
        }
        McpFormat::Codex => {
            if s.remote() {
                o.insert("url".into(), json!(s.url));
                opt(&mut o, "http_headers", &s.headers);
            } else {
                o.insert("command".into(), json!(s.command));
                o.insert("args".into(), json!(s.args));
                opt(&mut o, "env", &s.env);
            }
        }
    }
    Value::Object(o)
}

fn get_str(m: &Map<String, Value>, k: &str) -> String {
    m.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn get_list(m: &Map<String, Value>, k: &str) -> Vec<String> {
    m.get(k)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn get_map(m: &Map<String, Value>, k: &str) -> BTreeMap<String, String> {
    m.get(k)
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The server an agent's entry describes, or None for one siu can't read.
pub fn decode(format: McpFormat, name: &str, v: &Value) -> Option<Server> {
    let m = v.as_object()?;
    let mut s = Server {
        name: name.to_string(),
        ..Default::default()
    };
    let remote = |s: &mut Server, t: Transport, url: String, hk: &str| {
        s.transport = t;
        s.url = url;
        s.headers = get_map(m, hk);
    };
    let local = |s: &mut Server, ek: &str| {
        s.transport = Transport::Stdio;
        s.command = get_str(m, "command");
        s.args = get_list(m, "args");
        s.env = get_map(m, ek);
    };
    match format {
        McpFormat::OpenCode => {
            if get_str(m, "type") == "remote" {
                remote(&mut s, Transport::Http, get_str(m, "url"), "headers");
            } else {
                let mut cmd = get_list(m, "command");
                if cmd.is_empty() {
                    return None;
                }
                s.command = cmd.remove(0);
                s.args = cmd;
                s.env = get_map(m, "environment");
            }
        }
        McpFormat::Gemini => {
            let http = get_str(m, "httpUrl");
            let url = get_str(m, "url");
            if !http.is_empty() {
                remote(&mut s, Transport::Http, http, "headers");
            } else if !url.is_empty() {
                let t = if get_str(m, "type") == "http" {
                    Transport::Http
                } else {
                    Transport::Sse
                };
                remote(&mut s, t, url, "headers");
            } else {
                local(&mut s, "env");
            }
        }
        McpFormat::Codex => {
            let url = get_str(m, "url");
            if url.is_empty() {
                local(&mut s, "env");
            } else {
                remote(&mut s, Transport::Http, url, "http_headers");
            }
        }
        McpFormat::Claude | McpFormat::Cursor | McpFormat::Desktop | McpFormat::Copilot => {
            let url = get_str(m, "url");
            if url.is_empty() {
                local(&mut s, "env");
            } else {
                let t = if get_str(m, "type") == "sse" {
                    Transport::Sse
                } else {
                    Transport::Http
                };
                remote(&mut s, t, url, "headers");
            }
        }
    }
    if s.remote() && s.url.is_empty() || !s.remote() && s.command.is_empty() {
        return None;
    }
    Some(s)
}

// ---- files ------------------------------------------------------------------

/// Every server in an agent's file, by name; empty when there is no file.
pub fn read(file: &McpFile) -> Result<BTreeMap<String, Server>> {
    let mut out = BTreeMap::new();
    for (name, v) in read_raw(file)? {
        if let Some(s) = decode(file.format, &name, &v) {
            out.insert(name, s);
        }
    }
    Ok(out)
}

fn read_raw(file: &McpFile) -> Result<Vec<(String, Value)>> {
    let Some(text) = read_text(&file.path)? else {
        return Ok(vec![]);
    };
    let entries = if file.format == McpFormat::Codex {
        let doc: toml_edit::DocumentMut = text
            .parse()
            .with_context(|| format!("{}", file.path.display()))?;
        doc.get("mcp_servers")
            .and_then(|i| i.as_table_like())
            .map(|t| {
                t.iter()
                    .map(|(k, v)| (k.to_string(), toml_to_json(v)))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        let doc = parse_json(&text).with_context(|| format!("{}", file.path.display()))?;
        doc.get(container_key(file.format))
            .and_then(Value::as_object)
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    };
    Ok(entries)
}

fn read_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Ok(None),
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("{}", path.display())),
    }
}

/// Writes `put` into the file and takes `remove` out of it, in one go.
/// Nothing is written when nothing would change.
pub fn write(file: &McpFile, put: &[&Server], remove: &[String]) -> Result<bool> {
    let text = read_text(&file.path)?;
    let out = if file.format == McpFormat::Codex {
        write_codex(text.as_deref().unwrap_or(""), put, remove)?
    } else {
        write_json(file.format, text.as_deref(), put, remove)?
    };
    match out {
        Some(new) if Some(&new) != text.as_ref() => {
            write_atomic(&file.path, new.as_bytes())?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn write_json(
    format: McpFormat,
    text: Option<&str>,
    put: &[&Server],
    remove: &[String],
) -> Result<Option<String>> {
    let mut doc = match text {
        Some(t) => {
            if strip_jsonc(t) != t {
                bail!("has comments, which siu won't overwrite; edit it by hand");
            }
            parse_json(t)?
        }
        None => Value::Object(Map::new()),
    };
    let root = doc
        .as_object_mut()
        .ok_or_else(|| anyhow!("isn't a JSON object"))?;
    let key = container_key(format);
    let had = root.contains_key(key);
    let servers = root
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("{key} isn't an object"))?;
    let mut changed = false;
    for s in put {
        let new = merged(format, servers.get(&s.name), s);
        if servers.get(&s.name) != Some(&new) {
            servers.insert(s.name.clone(), new);
            changed = true;
        }
    }
    for name in remove {
        changed |= servers.shift_remove(name).is_some();
    }
    if !changed {
        return Ok(None);
    }
    if !had && servers.is_empty() {
        root.remove(key);
    }
    let mut out = serde_json::to_string_pretty(&doc)?;
    out.push('\n');
    Ok(Some(out))
}

/// The entry for `s`, keeping keys siu doesn't manage (timeouts, `disabled`,
/// trust settings) from the entry already there.
fn merged(format: McpFormat, old: Option<&Value>, s: &Server) -> Value {
    let mut new = encode(format, s);
    if let (Some(Value::Object(old)), Value::Object(n)) = (old, &mut new) {
        const MANAGED: &[&str] = &[
            "type",
            "url",
            "httpUrl",
            "headers",
            "http_headers",
            "command",
            "args",
            "env",
            "environment",
        ];
        for (k, v) in old {
            if !MANAGED.contains(&k.as_str()) && !n.contains_key(k) {
                n.insert(k.clone(), v.clone());
            }
        }
    }
    new
}

fn write_codex(text: &str, put: &[&Server], remove: &[String]) -> Result<Option<String>> {
    let mut doc: toml_edit::DocumentMut = text.parse()?;
    let before = doc.to_string();
    if !doc.contains_key("mcp_servers") {
        if put.is_empty() {
            return Ok(None);
        }
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        doc.insert("mcp_servers", toml_edit::Item::Table(t));
    }
    let servers = doc["mcp_servers"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("mcp_servers isn't a table"))?;
    for s in put {
        let old = servers.get(&s.name).map(toml_to_json);
        let want = merged(McpFormat::Codex, old.as_ref(), s);
        if old.as_ref() == Some(&want) {
            continue;
        }
        let mut t = servers
            .get(&s.name)
            .and_then(|i| i.as_table().cloned())
            .unwrap_or_default();
        // replace the managed keys, keep the others where they are
        for k in ["url", "http_headers", "command", "args", "env"] {
            t.remove(k);
        }
        if let Value::Object(o) = &want {
            for (k, v) in o {
                if !t.contains_key(k) {
                    t.insert(k, json_to_toml(v));
                }
            }
        }
        servers.insert(&s.name, toml_edit::Item::Table(t));
    }
    for name in remove {
        servers.remove(name);
    }
    let out = doc.to_string();
    Ok((out != before).then_some(out))
}

fn toml_to_json(item: &toml_edit::Item) -> Value {
    match item {
        toml_edit::Item::Value(v) => toml_value_to_json(v),
        toml_edit::Item::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_string(), toml_to_json(v)))
                .collect(),
        ),
        _ => Value::Null,
    }
}

fn toml_value_to_json(v: &toml_edit::Value) -> Value {
    use toml_edit::Value as T;
    match v {
        T::String(s) => json!(s.value()),
        T::Integer(i) => json!(i.value()),
        T::Float(f) => json!(f.value()),
        T::Boolean(b) => json!(b.value()),
        T::Datetime(d) => json!(d.value().to_string()),
        T::Array(a) => Value::Array(a.iter().map(toml_value_to_json).collect()),
        T::InlineTable(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_string(), toml_value_to_json(v)))
                .collect(),
        ),
    }
}

fn json_to_toml(v: &Value) -> toml_edit::Item {
    toml_edit::Item::Value(json_to_toml_value(v))
}

fn json_to_toml_value(v: &Value) -> toml_edit::Value {
    match v {
        Value::String(s) => s.as_str().into(),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => n
            .as_i64()
            .map(Into::into)
            .unwrap_or_else(|| n.as_f64().unwrap_or(0.0).into()),
        Value::Array(a) => toml_edit::Value::Array(a.iter().map(json_to_toml_value).collect()),
        Value::Object(o) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, v) in o {
                t.insert(k, json_to_toml_value(v));
            }
            toml_edit::Value::InlineTable(t)
        }
        Value::Null => "".into(),
    }
}

fn parse_json(text: &str) -> Result<Value> {
    Ok(serde_json::from_str(&strip_jsonc(text))?)
}

/// The text without // and /* */ comments or trailing commas.
pub fn strip_jsonc(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            let ch_len = utf8_len(c);
            out.push_str(&text[i..i + ch_len]);
            if c == b'\\' && i + 1 < b.len() {
                let n = utf8_len(b[i + 1]);
                out.push_str(&text[i + 1..i + 1 + n]);
                i += 1 + n;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += ch_len;
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                out.push('"');
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b',' => {
                // drop a comma that only closes the list or object
                if !matches!(next_significant(&text[i + 1..]), Some('}' | ']')) {
                    out.push(',');
                }
                i += 1;
            }
            _ => {
                let n = utf8_len(c);
                out.push_str(&text[i..i + n]);
                i += n;
            }
        }
    }
    out
}

/// The first character that isn't blank or inside a comment.
fn next_significant(mut rest: &str) -> Option<char> {
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("//") {
            rest = r.split_once('\n').map_or("", |(_, after)| after);
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, after)| after);
        } else {
            return rest.chars().next();
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Writes through a symlink to its target, by way of a temporary file.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!(
        "{}siu-tmp",
        path.extension()
            .map(|e| format!("{}.", e.to_string_lossy()))
            .unwrap_or_default()
    ));
    fs::write(&tmp, data).with_context(|| format!("{}", tmp.display()))?;
    if let Ok(meta) = fs::metadata(&path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, &path).with_context(|| format!("{}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn stdio() -> Server {
        Server {
            name: "fs".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "@mcp/fs".into()],
            env: [("TOKEN".to_string(), "x".to_string())].into(),
            ..Default::default()
        }
    }

    fn http() -> Server {
        Server {
            name: "ctx".into(),
            transport: Transport::Http,
            url: "https://mcp.example.com/mcp".into(),
            headers: [("Authorization".to_string(), "Bearer t".to_string())].into(),
            ..Default::default()
        }
    }

    fn sse() -> Server {
        Server {
            name: "old".into(),
            transport: Transport::Sse,
            url: "https://x.dev/sse".into(),
            ..Default::default()
        }
    }

    const ALL: [McpFormat; 7] = [
        McpFormat::Claude,
        McpFormat::Cursor,
        McpFormat::Desktop,
        McpFormat::Codex,
        McpFormat::Gemini,
        McpFormat::OpenCode,
        McpFormat::Copilot,
    ];

    #[test]
    fn every_format_reads_back_what_it_writes() {
        for f in ALL {
            for s in [stdio(), http(), sse()] {
                if unsupported(f, &s).is_some() {
                    continue;
                }
                let back = decode(f, &s.name, &encode(f, &s));
                let mut want = s.clone();
                // these spell every remote server the same way
                let one_remote = matches!(
                    f,
                    McpFormat::OpenCode | McpFormat::Cursor | McpFormat::Codex
                );
                if one_remote && want.transport == Transport::Sse {
                    want.transport = Transport::Http;
                }
                assert_eq!(back, Some(want), "{f:?}");
            }
        }
    }

    #[test]
    fn agents_spell_entries_their_own_way() {
        assert_eq!(
            encode(McpFormat::Gemini, &http())["httpUrl"],
            "https://mcp.example.com/mcp"
        );
        assert_eq!(
            encode(McpFormat::OpenCode, &stdio())["command"],
            json!(["npx", "-y", "@mcp/fs"])
        );
        assert_eq!(
            encode(McpFormat::OpenCode, &stdio())["environment"]["TOKEN"],
            "x"
        );
        assert_eq!(encode(McpFormat::Copilot, &stdio())["type"], "local");
        assert_eq!(encode(McpFormat::Claude, &sse())["type"], "sse");
        assert_eq!(
            encode(McpFormat::Codex, &http())["http_headers"]["Authorization"],
            "Bearer t"
        );
        assert!(unsupported(McpFormat::Desktop, &http()).is_some());
        assert!(unsupported(McpFormat::Codex, &sse()).is_some());
        assert!(unsupported(McpFormat::Codex, &http()).is_none());
    }

    fn file(dir: &Path, name: &str, format: McpFormat) -> McpFile {
        McpFile {
            path: dir.join(name),
            format,
        }
    }

    #[test]
    fn json_write_keeps_everything_else() {
        let d = tempfile::tempdir().unwrap();
        let f = file(d.path(), "claude.json", McpFormat::Claude);
        fs::write(
            &f.path,
            r#"{"zeta":1,"mcpServers":{"mine":{"type":"stdio","command":"me","args":[],"env":{},"timeout":5}},"alpha":{"x":[1,2]}}"#,
        )
        .unwrap();
        assert!(write(&f, &[&stdio()], &[]).unwrap());
        let v: Value = serde_json::from_str(&fs::read_to_string(&f.path).unwrap()).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["zeta", "mcpServers", "alpha"]);
        assert_eq!(v["mcpServers"]["mine"]["timeout"], 5);
        assert_eq!(v["mcpServers"]["fs"]["command"], "npx");

        // writing the same again changes nothing
        assert!(!write(&f, &[&stdio()], &[]).unwrap());
        assert!(write(&f, &[], &["fs".into()]).unwrap());
        let servers = read(&f).unwrap();
        assert_eq!(servers.keys().collect::<Vec<_>>(), ["mine"]);
    }

    #[test]
    fn unmanaged_keys_of_an_entry_survive_an_update() {
        let d = tempfile::tempdir().unwrap();
        let f = file(d.path(), "settings.json", McpFormat::Gemini);
        fs::write(
            &f.path,
            r#"{"mcpServers":{"fs":{"command":"old","args":[],"trust":true}}}"#,
        )
        .unwrap();
        write(&f, &[&stdio()], &[]).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&f.path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["fs"]["trust"], true);
        assert_eq!(v["mcpServers"]["fs"]["command"], "npx");
    }

    #[test]
    fn missing_file_is_created_and_jsonc_with_comments_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let f = file(&d.path().join("new"), "opencode.json", McpFormat::OpenCode);
        write(&f, &[&http()], &[]).unwrap();
        assert_eq!(read(&f).unwrap()["ctx"].url, "https://mcp.example.com/mcp");

        let c = file(d.path(), "opencode.jsonc", McpFormat::OpenCode);
        fs::write(&c.path, "{\n  // mine\n  \"mcp\": {\"a\": {\"type\":\"remote\",\"url\":\"https://a.dev\",},},\n}").unwrap();
        assert_eq!(read(&c).unwrap()["a"].url, "https://a.dev");
        let err = write(&c, &[&stdio()], &[]).unwrap_err().to_string();
        assert!(err.contains("comments"), "{err}");
    }

    #[test]
    fn codex_toml_keeps_comments_and_other_tables() {
        let d = tempfile::tempdir().unwrap();
        let f = file(d.path(), "config.toml", McpFormat::Codex);
        fs::write(
            &f.path,
            "# my config\nmodel = \"gpt-5\"\n\n[mcp_servers.mine]\ncommand = \"me\" # keep\ntool_timeout_sec = 90\n\n[profiles.fast]\nmodel = \"x\"\n",
        )
        .unwrap();
        let mut mine = Server {
            name: "mine".into(),
            command: "me2".into(),
            ..Default::default()
        };
        write(&f, &[&stdio(), &http(), &mine], &[]).unwrap();
        let text = fs::read_to_string(&f.path).unwrap();
        assert!(text.starts_with("# my config\nmodel = \"gpt-5\""), "{text}");
        assert!(text.contains("[profiles.fast]"));
        assert!(text.contains("tool_timeout_sec = 90"));
        let servers = read(&f).unwrap();
        assert_eq!(servers["fs"], stdio());
        assert_eq!(servers["ctx"], http());
        assert_eq!(servers["mine"].command, "me2");

        mine.command = "me2".into();
        assert!(!write(&f, &[&mine], &[]).unwrap());
        write(&f, &[], &["fs".into(), "ctx".into()]).unwrap();
        assert_eq!(read(&f).unwrap().keys().collect::<Vec<_>>(), ["mine"]);
    }

    #[test]
    fn writes_follow_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real.json");
        fs::write(&real, "{}").unwrap();
        let link = d.path().join("link.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(unix)]
        {
            write(
                &McpFile {
                    path: link.clone(),
                    format: McpFormat::Cursor,
                },
                &[&stdio()],
                &[],
            )
            .unwrap();
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert!(fs::read_to_string(&real).unwrap().contains("npx"));
        }
        let _ = PathBuf::new();
    }

    #[test]
    fn jsonc_stripping_respects_strings() {
        let t = r#"{"u": "http://x//y", /* c */ "a": [1, 2,], // end
}"#;
        let v: Value = serde_json::from_str(&strip_jsonc(t)).unwrap();
        assert_eq!(v["u"], "http://x//y");
        assert_eq!(v["a"], json!([1, 2]));
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_name("context7").is_ok());
        assert!(valid_name("a_b-c").is_ok());
        assert!(valid_name("-x").is_err());
        assert!(valid_name("a b").is_err());
        assert!(valid_name("").is_err());
    }
}
