//! Finding MCP servers and skills online: a hand-picked list of servers,
//! the official MCP Registry, and skills.sh.

use std::collections::BTreeSet;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use crate::mcp::{Server, Transport};
use crate::net;

const REGISTRY: &str = "https://registry.modelcontextprotocol.io/v0/servers";
const SKILLS_SH: &str = "https://www.skills.sh";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Env,
    Header,
    Arg,
}

/// Something a server needs from the user before it can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub key: String,
    pub place: Place,
    pub label: String,
    pub hint: String,
    pub secret: bool,
    pub required: bool,
    /// The value around what's typed, "Bearer {}" for a token header.
    pub format: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketServer {
    pub id: String,
    pub title: String,
    pub description: String,
    pub publisher: String,
    pub homepage: String,
    pub server: Server,
    pub inputs: Vec<Input>,
    pub featured: bool,
}

impl MarketServer {
    /// The server with what the user typed for each input filled in;
    /// an empty optional input is left out.
    pub fn fill(&self, values: &[String], home: &std::path::Path) -> Server {
        let mut s = self.server.clone();
        for (input, v) in self.inputs.iter().zip(values) {
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            match input.place {
                Place::Env => {
                    s.env.insert(input.key.clone(), v.to_string());
                }
                Place::Header => {
                    s.headers
                        .insert(input.key.clone(), input.format.replace("{}", v));
                }
                Place::Arg => {
                    let v = match v.strip_prefix("~/") {
                        Some(rest) => home.join(rest).to_string_lossy().to_string(),
                        None => v.to_string(),
                    };
                    s.args.push(v);
                }
            }
        }
        s
    }
}

fn secret_env(key: &str, hint: &str) -> Input {
    Input {
        key: key.into(),
        place: Place::Env,
        label: key.into(),
        hint: hint.into(),
        secret: true,
        required: true,
        format: "{}".into(),
    }
}

fn remote(
    id: &str,
    title: &str,
    publisher: &str,
    url: &str,
    desc: &str,
    inputs: Vec<Input>,
) -> MarketServer {
    MarketServer {
        id: id.into(),
        title: title.into(),
        description: desc.into(),
        publisher: publisher.into(),
        homepage: url.into(),
        server: Server {
            name: id.into(),
            transport: Transport::Http,
            url: url.into(),
            ..Default::default()
        },
        inputs,
        featured: true,
    }
}

#[allow(clippy::too_many_arguments)]
fn local(
    id: &str,
    title: &str,
    publisher: &str,
    home: &str,
    desc: &str,
    cmd: &str,
    args: &[&str],
    inputs: Vec<Input>,
) -> MarketServer {
    MarketServer {
        id: id.into(),
        title: title.into(),
        description: desc.into(),
        publisher: publisher.into(),
        homepage: home.into(),
        server: Server {
            name: id.into(),
            command: cmd.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        },
        inputs,
        featured: true,
    }
}

/// Servers worth offering first: well known, maintained, checked to work.
pub fn featured() -> Vec<MarketServer> {
    let mcp = "https://github.com/modelcontextprotocol/servers/tree/main/src";
    vec![
        remote(
            "context7",
            "Context7",
            "Upstash",
            "https://mcp.context7.com/mcp",
            "Up-to-date documentation and code examples for any library, straight into the prompt.",
            vec![Input {
                key: "CONTEXT7_API_KEY".into(),
                place: Place::Header,
                label: "API key".into(),
                hint: "Optional — raises the rate limit. From context7.com/dashboard".into(),
                secret: true,
                required: false,
                format: "{}".into(),
            }],
        ),
        local(
            "playwright",
            "Playwright",
            "Microsoft",
            "https://github.com/microsoft/playwright-mcp",
            "Drive a real browser: open pages, click, type and read them through the accessibility tree.",
            "npx",
            &["-y", "@playwright/mcp@latest"],
            vec![],
        ),
        local(
            "chrome-devtools",
            "Chrome DevTools",
            "Google",
            "https://github.com/ChromeDevTools/chrome-devtools-mcp",
            "Control and inspect a live Chrome: console, network, performance traces and screenshots.",
            "npx",
            &["-y", "chrome-devtools-mcp@latest"],
            vec![],
        ),
        remote(
            "github",
            "GitHub",
            "GitHub",
            "https://api.githubcopilot.com/mcp/",
            "Repositories, issues, pull requests, Actions and code search on GitHub.",
            vec![Input {
                key: "Authorization".into(),
                place: Place::Header,
                label: "Personal access token".into(),
                hint: "github.com/settings/personal-access-tokens".into(),
                secret: true,
                required: true,
                format: "Bearer {}".into(),
            }],
        ),
        remote(
            "deepwiki",
            "DeepWiki",
            "Cognition",
            "https://mcp.deepwiki.com/mcp",
            "Ask questions about any public GitHub repository and read its generated wiki.",
            vec![],
        ),
        remote(
            "exa",
            "Exa",
            "Exa",
            "https://mcp.exa.ai/mcp",
            "Web search and code search built for agents, with clean page contents.",
            vec![],
        ),
        local(
            "filesystem",
            "Filesystem",
            "Model Context Protocol",
            &format!("{mcp}/filesystem"),
            "Read, write and search files, limited to the folders you allow.",
            "npx",
            &["-y", "@modelcontextprotocol/server-filesystem"],
            vec![Input {
                key: "dir".into(),
                place: Place::Arg,
                label: "Folder".into(),
                hint: "The folder it may read and write, e.g. ~/code".into(),
                secret: false,
                required: true,
                format: "{}".into(),
            }],
        ),
        local(
            "fetch",
            "Fetch",
            "Model Context Protocol",
            &format!("{mcp}/fetch"),
            "Fetch a web page and hand it over as Markdown.",
            "uvx",
            &["mcp-server-fetch"],
            vec![],
        ),
        local(
            "memory",
            "Memory",
            "Model Context Protocol",
            &format!("{mcp}/memory"),
            "A knowledge graph the agent keeps across conversations.",
            "npx",
            &["-y", "@modelcontextprotocol/server-memory"],
            vec![],
        ),
        local(
            "sequential-thinking",
            "Sequential Thinking",
            "Model Context Protocol",
            &format!("{mcp}/sequentialthinking"),
            "Think a problem through step by step, revising and branching as it goes.",
            "npx",
            &["-y", "@modelcontextprotocol/server-sequential-thinking"],
            vec![],
        ),
        local(
            "git",
            "Git",
            "Model Context Protocol",
            &format!("{mcp}/git"),
            "Read, search and change Git repositories.",
            "uvx",
            &["mcp-server-git"],
            vec![],
        ),
        local(
            "time",
            "Time",
            "Model Context Protocol",
            &format!("{mcp}/time"),
            "The current time anywhere, and conversions between time zones.",
            "uvx",
            &["mcp-server-time"],
            vec![],
        ),
        remote(
            "sentry",
            "Sentry",
            "Sentry",
            "https://mcp.sentry.dev/mcp",
            "Issues, errors and traces from Sentry. Signs in through the browser on first use.",
            vec![],
        ),
        remote(
            "linear",
            "Linear",
            "Linear",
            "https://mcp.linear.app/mcp",
            "Find, create and update Linear issues, projects and comments. Signs in on first use.",
            vec![],
        ),
        remote(
            "notion",
            "Notion",
            "Notion",
            "https://mcp.notion.com/mcp",
            "Search, read and write pages and databases in your Notion workspace. Signs in on first use.",
            vec![],
        ),
        remote(
            "atlassian",
            "Atlassian",
            "Atlassian",
            "https://mcp.atlassian.com/v1/mcp",
            "Jira issues and Confluence pages from Atlassian Cloud. Signs in on first use.",
            vec![],
        ),
        remote(
            "supabase",
            "Supabase",
            "Supabase",
            "https://mcp.supabase.com/mcp",
            "Manage Supabase projects: tables, SQL, migrations, logs and edge functions.",
            vec![],
        ),
        remote(
            "stripe",
            "Stripe",
            "Stripe",
            "https://mcp.stripe.com",
            "Customers, payments, subscriptions and Stripe's documentation.",
            vec![],
        ),
        remote(
            "vercel",
            "Vercel",
            "Vercel",
            "https://mcp.vercel.com",
            "Projects, deployments and logs on Vercel, and its documentation.",
            vec![],
        ),
        remote(
            "neon",
            "Neon",
            "Neon",
            "https://mcp.neon.tech/mcp",
            "Serverless Postgres on Neon: projects, branches and SQL.",
            vec![],
        ),
        remote(
            "cloudflare-docs",
            "Cloudflare Docs",
            "Cloudflare",
            "https://docs.mcp.cloudflare.com/mcp",
            "Search Cloudflare's documentation.",
            vec![],
        ),
        remote(
            "microsoft-learn",
            "Microsoft Learn",
            "Microsoft",
            "https://learn.microsoft.com/api/mcp",
            "Official Microsoft and Azure documentation and code samples.",
            vec![],
        ),
        remote(
            "huggingface",
            "Hugging Face",
            "Hugging Face",
            "https://huggingface.co/mcp",
            "Search models, datasets, Spaces and papers on the Hugging Face Hub.",
            vec![],
        ),
        local(
            "brave-search",
            "Brave Search",
            "Brave",
            "https://github.com/brave/brave-search-mcp-server",
            "Web, news, image and local search from Brave's independent index.",
            "npx",
            &["-y", "@brave/brave-search-mcp-server"],
            vec![secret_env("BRAVE_API_KEY", "brave.com/search/api")],
        ),
        local(
            "firecrawl",
            "Firecrawl",
            "Firecrawl",
            "https://github.com/firecrawl/firecrawl-mcp-server",
            "Scrape, crawl and search the web into clean Markdown.",
            "npx",
            &["-y", "firecrawl-mcp"],
            vec![secret_env(
                "FIRECRAWL_API_KEY",
                "firecrawl.dev/app/api-keys",
            )],
        ),
        local(
            "tavily",
            "Tavily",
            "Tavily",
            "https://github.com/tavily-ai/tavily-mcp",
            "Search and extract from the web with Tavily.",
            "npx",
            &["-y", "tavily-mcp"],
            vec![secret_env("TAVILY_API_KEY", "app.tavily.com")],
        ),
    ]
}

/// The featured servers a search matches.
pub fn featured_matching(q: &str) -> Vec<MarketServer> {
    let q = q.trim().to_lowercase();
    featured()
        .into_iter()
        .filter(|m| {
            q.is_empty()
                || format!("{} {} {} {}", m.id, m.title, m.publisher, m.description)
                    .to_lowercase()
                    .contains(&q)
        })
        .collect()
}

// ---- the MCP Registry ---------------------------------------------------------

/// What the registry finds for a search, the featured matches left out.
pub fn search_registry(q: &str) -> Result<Vec<MarketServer>> {
    let url = format!(
        "{REGISTRY}?search={}&limit=60&version=latest",
        net::encode(q.trim())
    );
    let body: Value = net::get_json(&url)?;
    Ok(parse_registry(&body, q))
}

pub fn parse_registry(body: &Value, q: &str) -> Vec<MarketServer> {
    let featured: BTreeSet<String> = featured().into_iter().map(|m| m.server.summary()).collect();
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for e in body["servers"].as_array().into_iter().flatten() {
        let status = e["_meta"]["io.modelcontextprotocol.registry/official"]["status"]
            .as_str()
            .unwrap_or("active");
        if status != "active" {
            continue;
        }
        let Some(m) = from_registry(&e["server"]) else {
            continue;
        };
        if featured.contains(&m.server.summary()) || !seen.insert(m.server.summary()) {
            continue;
        }
        out.push(m);
    }
    let q = q.trim().to_lowercase();
    // a name that is the search comes first, then one that has it
    out.sort_by_key(|m| {
        let n = m.server.name.to_lowercase();
        if n == q {
            0
        } else if n.contains(&q) {
            1
        } else {
            2
        }
    });
    out
}

fn has_placeholder(s: &str) -> bool {
    s.contains('{') && s.contains('}')
}

/// A registry entry as a server: its remote when it has one, else the
/// package it's published as.
fn from_registry(r: &Value) -> Option<MarketServer> {
    let id = r["name"].as_str()?.to_string();
    let str_of = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let repo = str_of(&r["repository"]["url"]);
    let mut homepage = str_of(&r["websiteUrl"]);
    if homepage.is_empty() {
        homepage = repo.clone();
    }
    let publisher = match repo.strip_prefix("https://github.com/") {
        Some(rest) => rest.split('/').next().unwrap_or_default().to_string(),
        None => {
            let ns = id.split('/').next().unwrap_or_default();
            ns.split('.').rev().collect::<Vec<_>>().join(".")
        }
    };
    let name = short_name(&id);
    let title = match str_of(&r["title"]) {
        t if t.is_empty() => name.clone(),
        t => t,
    };
    let mut m = MarketServer {
        id: id.clone(),
        title,
        description: str_of(&r["description"]),
        publisher,
        homepage,
        server: Server {
            name,
            ..Default::default()
        },
        inputs: vec![],
        featured: false,
    };
    for want in ["streamable-http", "sse"] {
        for rm in r["remotes"].as_array().into_iter().flatten() {
            let url = str_of(&rm["url"]);
            if rm["type"] != want || has_placeholder(&url) || !url.starts_with("https://") {
                continue;
            }
            m.server.transport = if want == "sse" {
                Transport::Sse
            } else {
                Transport::Http
            };
            m.server.url = url;
            for h in rm["headers"].as_array().into_iter().flatten() {
                let (key, value) = (str_of(&h["name"]), str_of(&h["value"]));
                let (required, secret) = (h["isRequired"] == true, h["isSecret"] == true);
                if !value.is_empty() && !has_placeholder(&value) {
                    m.server.headers.insert(key, value);
                } else if required || secret {
                    let format = if value.is_empty() {
                        "{}".to_string()
                    } else {
                        replace_placeholders(&value)
                    };
                    m.inputs.push(Input {
                        label: key.clone(),
                        key,
                        place: Place::Header,
                        hint: str_of(&h["description"]),
                        secret,
                        required,
                        format,
                    });
                }
            }
            return Some(m);
        }
    }
    for want in ["npm", "pypi", "oci"] {
        for p in r["packages"].as_array().into_iter().flatten() {
            let ident = str_of(&p["identifier"]);
            let transport = str_of(&p["transport"]["type"]);
            if p["registryType"] != want
                || ident.is_empty()
                || !(transport.is_empty() || transport == "stdio")
            {
                continue;
            }
            let s = &mut m.server;
            match want {
                "npm" => {
                    s.command = "npx".into();
                    s.args = vec!["-y".into(), ident.clone()];
                }
                "pypi" => {
                    s.command = "uvx".into();
                    s.args = vec![ident.clone()];
                }
                _ => {
                    s.command = "docker".into();
                    s.args = vec!["run".into(), "-i".into(), "--rm".into()];
                }
            }
            for e in p["environmentVariables"].as_array().into_iter().flatten() {
                let (required, secret) = (e["isRequired"] == true, e["isSecret"] == true);
                if !required && !secret {
                    continue;
                }
                let key = str_of(&e["name"]);
                if want == "oci" {
                    m.server.args.extend(["-e".to_string(), key.clone()]);
                }
                m.inputs.push(Input {
                    label: key.clone(),
                    key,
                    place: Place::Env,
                    hint: str_of(&e["description"]),
                    secret,
                    required,
                    format: "{}".into(),
                });
            }
            if want == "oci" {
                m.server.args.push(ident.clone());
            }
            for (i, a) in p["packageArguments"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let mut v = str_of(&a["value"]);
                if v.is_empty() {
                    v = str_of(&a["default"]);
                }
                if has_placeholder(&v) {
                    v.clear();
                }
                let named = a["type"] == "named";
                let required = a["isRequired"] == true;
                if !v.is_empty() && named {
                    m.server.args.extend([str_of(&a["name"]), v]);
                } else if !v.is_empty() {
                    m.server.args.push(v);
                } else if required && !named {
                    let mut label = str_of(&a["valueHint"]);
                    if label.is_empty() {
                        label = format!("Argument {}", i + 1);
                    }
                    m.inputs.push(Input {
                        key: format!("arg{i}"),
                        place: Place::Arg,
                        label,
                        hint: str_of(&a["description"]),
                        secret: false,
                        required: true,
                        format: "{}".into(),
                    });
                } else if required {
                    return None; // a named argument siu can't fill
                }
            }
            return Some(m);
        }
    }
    None
}

fn replace_placeholders(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '{' => {
                if depth == 0 {
                    out.push_str("{}");
                }
                depth += 1;
            }
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// A registry name as a library name: io.github.upstash/context7-mcp is context7.
pub fn short_name(n: &str) -> String {
    let orig = n.rsplit('/').next().unwrap_or(n).to_lowercase();
    let mut s = orig.as_str();
    for p in ["mcp-server-", "server-", "mcp-"] {
        s = s.strip_prefix(p).unwrap_or(s);
    }
    for suf in ["-mcp-server", "-mcp", "-server", "_mcp", ".mcp"] {
        s = s.strip_suffix(suf).unwrap_or(s);
    }
    let s = if s.is_empty() { orig.as_str() } else { s };
    let clean: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean
        .trim_matches(|c| c == '-' || c == '_')
        .chars()
        .take(64)
        .collect::<String>();
    if clean.is_empty() {
        "server".into()
    } else {
        clean
    }
}

// ---- skills.sh ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct MarketSkill {
    pub source: String,
    #[serde(rename = "skillId")]
    pub skill_id: String,
    pub name: String,
    #[serde(default)]
    pub installs: u64,
    #[serde(default, rename = "isOfficial")]
    pub official: bool,
}

impl MarketSkill {
    pub fn key(&self) -> String {
        format!("{}/{}", self.source, self.skill_id)
    }
}

fn is_repo(s: &str) -> bool {
    let parts: Vec<_> = s.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
}

pub fn search_skills(q: &str) -> Result<Vec<MarketSkill>> {
    #[derive(Deserialize)]
    struct Body {
        skills: Vec<MarketSkill>,
    }
    let body: Body = net::get_json(&format!(
        "{SKILLS_SH}/api/search?q={}&limit=50",
        net::encode(q.trim())
    ))?;
    Ok(body
        .skills
        .into_iter()
        .filter(|s| is_repo(&s.source) && !s.skill_id.is_empty())
        .collect())
}

/// skills.sh's most installed, read off its front page.
pub fn popular_skills() -> Result<Vec<MarketSkill>> {
    let page = String::from_utf8_lossy(&net::get(&format!("{SKILLS_SH}/"), "text/html", 8 << 20)?)
        .to_string();
    parse_popular(&page)
}

pub fn parse_popular(page: &str) -> Result<Vec<MarketSkill>> {
    const KEY: &str = r#"initialSkills\":"#;
    let Some(i) = page.find(KEY) else {
        anyhow::bail!("skills.sh's page has no list of skills")
    };
    let rest = page[i + KEY.len()..]
        .replace(r#"\\"#, "\\")
        .replace(r#"\""#, "\"");
    let mut de = serde_json::Deserializer::from_str(&rest);
    let list: Vec<MarketSkill> = serde::Deserialize::deserialize(&mut de)?;
    Ok(list
        .into_iter()
        .filter(|s| is_repo(&s.source) && !s.skill_id.is_empty())
        .collect())
}

/// The popular list as first shown: a few from each repository, for the
/// page not to be one author's.
pub fn popular_view(index: &[MarketSkill]) -> Vec<MarketSkill> {
    let mut per = std::collections::HashMap::<&str, usize>::new();
    index
        .iter()
        .filter(|s| {
            let n = per.entry(&s.source).or_default();
            *n += 1;
            *n <= 4
        })
        .take(60)
        .cloned()
        .collect()
}

/// The skills of `list` whose name, id or repository has every word of
/// the query, most installed first.
pub fn search_local(list: &[MarketSkill], q: &str, limit: usize) -> Vec<MarketSkill> {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return vec![];
    }
    let mut hits: Vec<&MarketSkill> = list
        .iter()
        .filter(|s| {
            let hay = format!("{} {} {}", s.name, s.skill_id, s.source).to_lowercase();
            words.iter().all(|w| hay.contains(w))
        })
        .collect();
    hits.sort_by_key(|s| std::cmp::Reverse(s.installs));
    hits.into_iter().take(limit).cloned().collect()
}

/// skills.sh's own results first, then the local ones it didn't have.
pub fn merge(remote: Vec<MarketSkill>, local: &[MarketSkill]) -> Vec<MarketSkill> {
    let keys: std::collections::HashSet<String> = remote.iter().map(MarketSkill::key).collect();
    let official: std::collections::HashSet<String> = local
        .iter()
        .filter(|s| s.official)
        .map(MarketSkill::key)
        .collect();
    let mut out: Vec<MarketSkill> = remote
        .into_iter()
        .map(|mut s| {
            s.official |= official.contains(&s.key());
            s
        })
        .collect();
    out.extend(local.iter().filter(|s| !keys.contains(&s.key())).cloned());
    out
}

/// What a skill says it's for, from its page on skills.sh.
pub fn skill_about(s: &MarketSkill) -> Option<String> {
    let url = format!("{SKILLS_SH}/{}/{}", s.source, net::encode(&s.skill_id));
    let page = net::get(&url, "text/html", 4 << 20).ok()?;
    meta_description(&String::from_utf8_lossy(&page))
}

pub fn meta_description(page: &str) -> Option<String> {
    let i = page.find(r#"<meta name="description" content=""#)?;
    let rest = &page[i + 34..];
    let d = &rest[..rest.find('"')?];
    let d = d
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    (!d.trim().is_empty()).then(|| d.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_entries_become_servers() {
        let body = json!({"servers": [
            {"server": {"name": "io.github.acme/weather-mcp", "description": "Weather",
                "repository": {"url": "https://github.com/acme/weather"},
                "packages": [{"registryType": "npm", "identifier": "@acme/weather", "transport": {"type": "stdio"},
                    "environmentVariables": [{"name": "API_KEY", "isRequired": true, "isSecret": true}, {"name": "DEBUG"}],
                    "packageArguments": [{"type": "positional", "valueHint": "city", "isRequired": true}]}]}},
            {"server": {"name": "com.example/remote", "title": "Remote",
                "remotes": [{"type": "streamable-http", "url": "https://x.example.com/mcp",
                    "headers": [{"name": "Authorization", "value": "Bearer {token}", "isRequired": true, "isSecret": true}]}]}},
            {"server": {"name": "com.example/gone", "remotes": [{"type": "sse", "url": "https://gone.dev"}]},
                "_meta": {"io.modelcontextprotocol.registry/official": {"status": "deleted"}}},
            {"server": {"name": "com.example/templated", "remotes": [{"type": "sse", "url": "https://{tenant}.dev"}]}},
            {"server": {"name": "io.github.upstash/context7", "remotes": [{"type": "streamable-http", "url": "https://mcp.context7.com/mcp"}]}}
        ]});
        let list = parse_registry(&body, "remote");
        assert_eq!(list.len(), 2, "{list:#?}");
        let r = &list[0];
        assert_eq!(r.server.name, "remote");
        assert_eq!(r.inputs[0].format, "Bearer {}");
        let filled = r.fill(&["tok".into()], std::path::Path::new("/h"));
        assert_eq!(filled.headers["Authorization"], "Bearer tok");

        let w = &list[1];
        assert_eq!(
            (w.server.name.as_str(), w.publisher.as_str()),
            ("weather", "acme")
        );
        assert_eq!(w.server.command, "npx");
        assert_eq!(w.inputs.len(), 2);
        let filled = w.fill(&["k".into(), "~/x".into()], std::path::Path::new("/h"));
        assert_eq!(filled.env["API_KEY"], "k");
        assert_eq!(filled.args, ["-y", "@acme/weather", "/h/x"]);
    }

    #[test]
    fn short_names() {
        assert_eq!(short_name("io.github.upstash/context7-mcp"), "context7");
        assert_eq!(short_name("ai.x/mcp-server-git"), "git");
        assert_eq!(short_name("a/mcp"), "mcp");
        assert_eq!(short_name("a/Foo Bar"), "foo-bar");
    }

    #[test]
    fn skills_sh_pages() {
        let page = r#"x self.__next_f.push([1,"{\"initialSkills\":[{\"source\":\"a/b\",\"skillId\":\"s1\",\"name\":\"s1\",\"installs\":10},{\"source\":\"bad\",\"skillId\":\"x\",\"name\":\"x\"}],\"more\":1}"])"#;
        let list = parse_popular(page).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].installs, 10);
        assert_eq!(list[0].key(), "a/b/s1");
        assert_eq!(
            meta_description(
                r#"<head><meta name="description" content="Make &amp; read PDFs"/></head>"#
            ),
            Some("Make & read PDFs".into())
        );
    }

    fn sk(source: &str, id: &str, installs: u64) -> MarketSkill {
        MarketSkill {
            source: source.into(),
            skill_id: id.into(),
            name: id.into(),
            installs,
            official: false,
        }
    }

    #[test]
    fn the_popular_index_is_kept_whole_and_shown_varied() {
        let mut index = vec![];
        for i in 0..10 {
            index.push(sk("a/many", &format!("a{i}"), 1000 - i));
        }
        for i in 0..80 {
            index.push(sk(&format!("o{i}/r"), "x", 500 - i));
        }
        let view = popular_view(&index);
        assert_eq!(view.iter().filter(|s| s.source == "a/many").count(), 4);
        assert_eq!(view.len(), 60);
        assert_eq!(view[0].skill_id, "a0");
    }

    #[test]
    fn local_search_matches_every_word_most_installed_first() {
        let index = vec![
            sk(
                "vercel-labs/agent-skills",
                "vercel-react-best-practices",
                700,
            ),
            sk("acme/react", "react-native", 900),
            sk("acme/x", "testing", 50),
            sk("facebook/react", "docs", 10),
        ];
        let keys = |v: Vec<MarketSkill>| v.into_iter().map(|s| s.skill_id).collect::<Vec<_>>();
        assert_eq!(
            keys(search_local(&index, "react", 10)),
            ["react-native", "vercel-react-best-practices", "docs"]
        );
        assert_eq!(
            keys(search_local(&index, "React  best", 10)),
            ["vercel-react-best-practices"]
        );
        assert_eq!(keys(search_local(&index, "react", 1)), ["react-native"]);
        assert!(search_local(&index, "", 10).is_empty());
    }

    #[test]
    fn remote_results_lead_and_local_ones_fill_in() {
        let remote = vec![sk("a/b", "one", 1), sk("a/b", "two", 2)];
        let local = vec![sk("a/b", "two", 2), sk("c/d", "three", 3)];
        let merged = merge(remote, &local);
        let ids: Vec<_> = merged.iter().map(|s| s.skill_id.as_str()).collect();
        assert_eq!(ids, ["one", "two", "three"]);

        // the search doesn't say what's official; the popular list does
        let mut official = sk("a/b", "one", 1);
        official.official = true;
        assert!(merge(vec![sk("a/b", "one", 1)], &[official])[0].official);
    }

    #[test]
    fn featured_search() {
        assert!(featured_matching("").len() > 20);
        let found = featured_matching("browser");
        assert!(found.iter().any(|m| m.id == "playwright"));
        for m in featured() {
            m.server.validate().unwrap();
        }
    }
}
