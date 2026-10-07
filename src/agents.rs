//! The agents siu knows: where each keeps its MCP servers and its skills.

use std::path::PathBuf;

/// How an agent's MCP config file spells a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpFormat {
    /// `mcpServers` with `type` stdio/http/sse (Claude Code, Droid).
    Claude,
    /// `mcpServers` with a bare command or url, stdio only for Claude Desktop.
    Cursor,
    Desktop,
    /// `[mcp_servers.<name>]` tables in TOML (Codex).
    Codex,
    /// `mcpServers` with `httpUrl` for streamable HTTP (Gemini CLI).
    Gemini,
    /// `mcp` with `type` local/remote and the command as one list (OpenCode).
    OpenCode,
    /// `mcpServers` with `type` local/http/sse and `tools` (Copilot CLI).
    Copilot,
}

#[derive(Debug, Clone)]
pub struct McpFile {
    pub path: PathBuf,
    pub format: McpFormat,
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub id: &'static str,
    pub name: &'static str,
    /// The folder whose presence says the agent is installed.
    pub home: PathBuf,
    pub mcp: Option<McpFile>,
    pub skills: Option<PathBuf>,
}

impl Agent {
    pub fn detected(&self) -> bool {
        self.home.is_dir()
    }
}

/// Where things live on this machine: the user's home, the platform config
/// folder (Claude Desktop's parent) and the overrides agents honour.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    pub config: PathBuf,
    pub claude_dir: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
}

impl Env {
    pub fn from_system() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Env {
            claude_dir: var("CLAUDE_CONFIG_DIR"),
            codex_home: var("CODEX_HOME"),
            home,
            config,
        }
    }

    /// An environment rooted entirely in `home`, for tests.
    #[cfg(test)]
    pub fn rooted(home: &std::path::Path) -> Self {
        Env {
            home: home.to_path_buf(),
            config: home.join(".config"),
            claude_dir: None,
            codex_home: None,
        }
    }
}

/// Every agent siu can manage.
pub fn all(env: &Env) -> Vec<Agent> {
    let home = &env.home;
    let config = &env.config;
    let h = |p: &str| home.join(p);
    let claude_dir = env.claude_dir.clone().unwrap_or_else(|| h(".claude"));
    let codex_dir = env.codex_home.clone().unwrap_or_else(|| h(".codex"));
    // Claude Code keeps its user servers in .claude.json beside its folder,
    // or inside it when CLAUDE_CONFIG_DIR moves it.
    let claude_json = match &env.claude_dir {
        Some(d) => d.join(".claude.json"),
        None => h(".claude.json"),
    };
    let opencode_dir = h(".config/opencode");
    let opencode_file = if opencode_dir.join("opencode.jsonc").is_file()
        && !opencode_dir.join("opencode.json").is_file()
    {
        opencode_dir.join("opencode.jsonc")
    } else {
        opencode_dir.join("opencode.json")
    };
    let desktop_dir = config.join("Claude");
    let mcp = |path: PathBuf, format| Some(McpFile { path, format });
    vec![
        Agent {
            id: "claude",
            name: "Claude Code",
            home: claude_dir.clone(),
            mcp: mcp(claude_json, McpFormat::Claude),
            skills: Some(claude_dir.join("skills")),
        },
        Agent {
            id: "codex",
            name: "Codex",
            home: codex_dir.clone(),
            mcp: mcp(codex_dir.join("config.toml"), McpFormat::Codex),
            skills: Some(codex_dir.join("skills")),
        },
        Agent {
            id: "gemini",
            name: "Gemini CLI",
            home: h(".gemini"),
            mcp: mcp(h(".gemini/settings.json"), McpFormat::Gemini),
            skills: Some(h(".gemini/skills")),
        },
        Agent {
            id: "opencode",
            name: "OpenCode",
            home: opencode_dir.clone(),
            mcp: mcp(opencode_file, McpFormat::OpenCode),
            skills: Some(opencode_dir.join("skills")),
        },
        Agent {
            id: "cursor",
            name: "Cursor",
            home: h(".cursor"),
            mcp: mcp(h(".cursor/mcp.json"), McpFormat::Cursor),
            skills: Some(h(".cursor/skills")),
        },
        Agent {
            id: "copilot",
            name: "Copilot CLI",
            home: h(".copilot"),
            mcp: mcp(h(".copilot/mcp-config.json"), McpFormat::Copilot),
            skills: Some(h(".copilot/skills")),
        },
        Agent {
            id: "droid",
            name: "Droid",
            home: h(".factory"),
            mcp: mcp(h(".factory/mcp.json"), McpFormat::Claude),
            skills: Some(h(".factory/skills")),
        },
        Agent {
            id: "claude-desktop",
            name: "Claude Desktop",
            home: desktop_dir.clone(),
            mcp: mcp(
                desktop_dir.join("claude_desktop_config.json"),
                McpFormat::Desktop,
            ),
            skills: None,
        },
        Agent {
            id: "agents",
            name: "~/.agents (shared)",
            home: h(".agents"),
            mcp: None,
            skills: Some(h(".agents/skills")),
        },
    ]
}
