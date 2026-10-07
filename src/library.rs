//! The library: every MCP server and skill siu manages, which agents get
//! each, and what siu last wrote into each agent so it only ever takes back
//! its own.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::agents::{self, Agent, Env};
use crate::mcp::{self, Server};
use crate::skills::{self, Found};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpEntry {
    #[serde(flatten)]
    pub server: Server,
    #[serde(default)]
    pub agents: BTreeSet<String>,
    /// Off keeps the agents picked but writes the server to none of them.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Where it was installed from: a registry name, "featured", "import".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    Github {
        repo: String,
        #[serde(rename = "ref")]
        reference: String,
        path: String,
    },
    Local {
        dir: PathBuf,
    },
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::Github { repo, path, .. } if path.is_empty() => format!("github.com/{repo}"),
            Source::Github { repo, path, .. } => format!("github.com/{repo}/{path}"),
            Source::Local { dir } => dir.display().to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(default)]
    pub agents: BTreeSet<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub installed: u64,
    #[serde(default)]
    pub updated: u64,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Applied {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub mcp: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub skills: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Library {
    #[serde(default)]
    pub mcp: Vec<McpEntry>,
    #[serde(default)]
    pub skills: Vec<SkillEntry>,
    #[serde(default)]
    pub applied: BTreeMap<String, Applied>,
}

/// A server in an agent's file that the library doesn't have.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundServer {
    pub server: Server,
    pub agents: Vec<String>,
}

/// A skill in an agent's folder that isn't one of the library's links.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundSkill {
    pub name: String,
    pub description: String,
    /// The agents that have it, and where.
    pub at: Vec<(String, PathBuf)>,
}

/// Whether an agent gets an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    On,
    Off,
    /// Picked, but the entry is paused.
    Paused,
    Unsupported(&'static str),
}

#[derive(Debug, Default)]
pub struct Report {
    pub changed: usize,
    pub issues: Vec<String>,
}

pub enum UpdateOutcome {
    Updated,
    UpToDate,
}

pub struct Store {
    pub root: PathBuf,
    pub agents: Vec<Agent>,
    pub lib: Library,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Store {
    pub fn open(env: &Env, root: PathBuf) -> Result<Self> {
        let path = root.join("library.json");
        let lib = match fs::read_to_string(&path) {
            Ok(t) => serde_json::from_str(&t).with_context(|| format!("{}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Library::default(),
            Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
        };
        Ok(Store {
            root,
            agents: agents::all(env),
            lib,
        })
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    pub fn skill_dir(&self, name: &str) -> PathBuf {
        self.skills_dir().join(name)
    }

    pub fn detected(&self) -> impl Iterator<Item = &Agent> {
        self.agents.iter().filter(|a| a.detected())
    }

    pub fn mcp_agents(&self) -> Vec<&Agent> {
        self.detected().filter(|a| a.mcp.is_some()).collect()
    }

    pub fn skill_agents(&self) -> Vec<&Agent> {
        self.detected().filter(|a| a.skills.is_some()).collect()
    }

    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }

    pub fn save(&mut self) -> Result<()> {
        self.lib
            .mcp
            .sort_by(|a, b| a.server.name.cmp(&b.server.name));
        self.lib.skills.sort_by(|a, b| a.name.cmp(&b.name));
        let mut text = serde_json::to_string_pretty(&self.lib)?;
        text.push('\n');
        mcp::write_atomic(&self.root.join("library.json"), text.as_bytes())
    }

    /// Saves the library and writes it into every agent.
    pub fn commit(&mut self) -> Result<Report> {
        self.save()?;
        let report = self.sync();
        self.save()?;
        Ok(report)
    }

    // ---- states -------------------------------------------------------------

    pub fn server_state(&self, e: &McpEntry, agent: &Agent) -> State {
        let Some(f) = &agent.mcp else {
            return State::Unsupported("no MCP config");
        };
        if let Some(why) = mcp::unsupported(f.format, &e.server) {
            return State::Unsupported(why);
        }
        match (e.agents.contains(agent.id), e.enabled) {
            (true, true) => State::On,
            (true, false) => State::Paused,
            _ => State::Off,
        }
    }

    pub fn skill_state(&self, e: &SkillEntry, agent: &Agent) -> State {
        if agent.skills.is_none() {
            return State::Unsupported("no skills folder");
        }
        match (e.agents.contains(agent.id), e.enabled) {
            (true, true) => State::On,
            (true, false) => State::Paused,
            _ => State::Off,
        }
    }

    // ---- MCP servers --------------------------------------------------------

    pub fn server(&self, name: &str) -> Option<&McpEntry> {
        self.lib.mcp.iter().find(|e| e.server.name == name)
    }

    fn server_mut(&mut self, name: &str) -> Result<&mut McpEntry> {
        match self.lib.mcp.iter_mut().find(|e| e.server.name == name) {
            Some(e) => Ok(e),
            None => bail!("no server called {name}"),
        }
    }

    /// Every detected agent that can reach the server.
    pub fn default_server_agents(&self, s: &Server) -> BTreeSet<String> {
        self.mcp_agents()
            .into_iter()
            .filter(|a| {
                a.mcp
                    .as_ref()
                    .is_some_and(|f| mcp::unsupported(f.format, s).is_none())
            })
            .map(|a| a.id.to_string())
            .collect()
    }

    pub fn add_server(
        &mut self,
        server: Server,
        agents: BTreeSet<String>,
        origin: Option<String>,
    ) -> Result<()> {
        server.validate()?;
        if self.server(&server.name).is_some() {
            bail!("the library already has a server called {}", server.name);
        }
        self.lib.mcp.push(McpEntry {
            server,
            agents,
            enabled: true,
            origin,
        });
        Ok(())
    }

    /// Replaces a server's definition, renaming it when its name changed.
    pub fn edit_server(&mut self, old_name: &str, server: Server) -> Result<()> {
        server.validate()?;
        if server.name != old_name && self.server(&server.name).is_some() {
            bail!("the library already has a server called {}", server.name);
        }
        let e = self.server_mut(old_name)?;
        e.server = server;
        Ok(())
    }

    pub fn toggle_server_agent(&mut self, name: &str, agent: &str) -> Result<bool> {
        let e = self.server_mut(name)?;
        let on = !e.agents.contains(agent);
        if on {
            e.agents.insert(agent.to_string());
            e.enabled = true;
        } else {
            e.agents.remove(agent);
        }
        Ok(on)
    }

    pub fn set_server_agents(&mut self, name: &str, agents: BTreeSet<String>) -> Result<()> {
        self.server_mut(name)?.agents = agents;
        Ok(())
    }

    pub fn toggle_server_enabled(&mut self, name: &str) -> Result<bool> {
        let e = self.server_mut(name)?;
        e.enabled = !e.enabled;
        Ok(e.enabled)
    }

    pub fn remove_server(&mut self, name: &str) -> Result<()> {
        let n = self.lib.mcp.len();
        self.lib.mcp.retain(|e| e.server.name != name);
        if n == self.lib.mcp.len() {
            bail!("no server called {name}");
        }
        Ok(())
    }

    /// Servers the agents have that the library doesn't, by name.
    pub fn found_servers(&self) -> Vec<FoundServer> {
        let mut out: BTreeMap<String, FoundServer> = BTreeMap::new();
        for a in self.mcp_agents() {
            let Ok(servers) = mcp::read(a.mcp.as_ref().unwrap()) else {
                continue;
            };
            for (name, s) in servers {
                if self.server(&name).is_some() {
                    continue;
                }
                out.entry(name)
                    .or_insert_with(|| FoundServer {
                        server: s,
                        agents: vec![],
                    })
                    .agents
                    .push(a.id.to_string());
            }
        }
        out.into_values().collect()
    }

    /// Takes a server the agents have into the library; from then on siu
    /// manages it in each of them.
    pub fn import_server(&mut self, found: &FoundServer) -> Result<()> {
        let agents: BTreeSet<String> = found.agents.iter().cloned().collect();
        self.add_server(found.server.clone(), agents.clone(), Some("import".into()))?;
        for a in agents {
            self.lib
                .applied
                .entry(a)
                .or_default()
                .mcp
                .insert(found.server.name.clone());
        }
        Ok(())
    }

    // ---- skills -------------------------------------------------------------

    pub fn skill(&self, name: &str) -> Option<&SkillEntry> {
        self.lib.skills.iter().find(|e| e.name == name)
    }

    fn skill_mut(&mut self, name: &str) -> Result<&mut SkillEntry> {
        match self.lib.skills.iter_mut().find(|e| e.name == name) {
            Some(e) => Ok(e),
            None => bail!("no skill called {name}"),
        }
    }

    pub fn default_skill_agents(&self) -> BTreeSet<String> {
        // ~/.agents is read by several agents besides their own folders: a
        // skill given there and to them would show up twice
        let own: Vec<_> = self
            .skill_agents()
            .into_iter()
            .filter(|a| a.id != "agents")
            .collect();
        let pick = if own.is_empty() {
            self.skill_agents()
        } else {
            own
        };
        pick.into_iter().map(|a| a.id.to_string()).collect()
    }

    /// Copies found skills into the library. `source` gives where the tree
    /// came from, so each can be updated from there later.
    pub fn install_skills(
        &mut self,
        found: &[Found],
        source: impl Fn(&Found) -> Option<Source>,
        agents: &BTreeSet<String>,
    ) -> Result<Vec<String>> {
        let mut names = vec![];
        for f in found {
            let name = f.name();
            mcp::valid_name(&name)?;
            if self.skill(&name).is_some() {
                bail!("the library already has a skill called {name}");
            }
            let dst = self.skill_dir(&name);
            skills::replace_dir(&f.dir, &dst)?;
            self.lib.skills.push(SkillEntry {
                name: name.clone(),
                description: f.meta.description.clone(),
                source: source(f),
                agents: agents.clone(),
                enabled: true,
                hash: skills::hash_dir(&dst),
                installed: now(),
                updated: now(),
            });
            names.push(name);
        }
        Ok(names)
    }

    /// Refreshes a skill from a fresh copy of its source tree.
    pub fn update_skill_from(&mut self, name: &str, tree: &Path) -> Result<UpdateOutcome> {
        let e = self
            .skill(name)
            .with_context(|| format!("no skill called {name}"))?
            .clone();
        let want = match &e.source {
            Some(Source::Github { path, .. }) => path.clone(),
            Some(Source::Local { .. }) => String::new(),
            None => bail!("{name} has no source to update from"),
        };
        let found = skills::find(tree);
        let f = found
            .iter()
            .find(|f| f.path == want)
            .or_else(|| found.iter().find(|f| f.name() == name))
            .with_context(|| format!("{name} is no longer at its source"))?;
        let dst = self.skill_dir(name);
        if skills::hash_dir(&f.dir) == skills::hash_dir(&dst) {
            return Ok(UpdateOutcome::UpToDate);
        }
        skills::replace_dir(&f.dir, &dst)?;
        let hash = skills::hash_dir(&dst);
        let s = self.skill_mut(name)?;
        s.hash = hash;
        s.updated = now();
        if !f.meta.description.is_empty() {
            s.description = f.meta.description.clone();
        }
        Ok(UpdateOutcome::Updated)
    }

    pub fn toggle_skill_agent(&mut self, name: &str, agent: &str) -> Result<bool> {
        let e = self.skill_mut(name)?;
        let on = !e.agents.contains(agent);
        if on {
            e.agents.insert(agent.to_string());
            e.enabled = true;
        } else {
            e.agents.remove(agent);
        }
        Ok(on)
    }

    pub fn set_skill_agents(&mut self, name: &str, agents: BTreeSet<String>) -> Result<()> {
        self.skill_mut(name)?.agents = agents;
        Ok(())
    }

    pub fn toggle_skill_enabled(&mut self, name: &str) -> Result<bool> {
        let e = self.skill_mut(name)?;
        e.enabled = !e.enabled;
        Ok(e.enabled)
    }

    /// Takes a skill out of the library; the sync then takes its links out
    /// of the agents.
    pub fn remove_skill(&mut self, name: &str) -> Result<()> {
        self.skill_mut(name)?;
        self.lib.skills.retain(|e| e.name != name);
        Ok(())
    }

    /// Skills in agents' folders that aren't the library's.
    pub fn found_skills(&self) -> Vec<FoundSkill> {
        let mut out: BTreeMap<String, FoundSkill> = BTreeMap::new();
        for a in self.skill_agents() {
            let dir = a.skills.as_ref().unwrap();
            let Ok(rd) = fs::read_dir(dir) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let p = e.path();
                if name.starts_with('.')
                    || !p.join("SKILL.md").is_file()
                    || self.skill(&name).is_some()
                {
                    continue;
                }
                if skills::links_to(&p, &self.skill_dir(&name)) {
                    continue;
                }
                let meta =
                    skills::parse_meta(&fs::read_to_string(p.join("SKILL.md")).unwrap_or_default());
                out.entry(name.clone())
                    .or_insert_with(|| FoundSkill {
                        name,
                        description: meta.description,
                        at: vec![],
                    })
                    .at
                    .push((a.id.to_string(), p));
            }
        }
        out.into_values().collect()
    }

    /// Takes a skill the agents have into the library: its files are copied
    /// in, and each agent's copy is set aside in backups and replaced by a
    /// link to the library's.
    pub fn import_skill(&mut self, found: &FoundSkill) -> Result<()> {
        let name = skills::sanitize(&found.name);
        mcp::valid_name(&name)?;
        if self.skill(&name).is_some() {
            bail!("the library already has a skill called {name}");
        }
        let (_, first) = found.at.first().context("found nowhere")?;
        let src = fs::canonicalize(first)?;
        let dst = self.skill_dir(&name);
        skills::replace_dir(&src, &dst)?;
        let stamp = now();
        let mut agents = BTreeSet::new();
        for (agent, p) in &found.at {
            let is_link = fs::symlink_metadata(p)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            if is_link {
                fs::remove_file(p)?;
            } else {
                let keep = self
                    .root
                    .join("backups")
                    .join("skills")
                    .join(agent)
                    .join(format!("{}-{stamp}", found.name));
                fs::create_dir_all(keep.parent().unwrap())?;
                fs::rename(p, &keep).or_else(|_| {
                    skills::copy_dir(p, &keep).and_then(|_| Ok(fs::remove_dir_all(p)?))
                })?;
            }
            agents.insert(agent.clone());
            self.lib
                .applied
                .entry(agent.clone())
                .or_default()
                .skills
                .insert(name.clone());
        }
        self.lib.skills.push(SkillEntry {
            name: name.clone(),
            description: found.description.clone(),
            source: None,
            agents,
            enabled: true,
            hash: skills::hash_dir(&dst),
            installed: stamp,
            updated: stamp,
        });
        Ok(())
    }

    // ---- writing into the agents --------------------------------------------

    /// Writes the library into every detected agent: what each should have
    /// is put in, what siu put there before and it shouldn't have any more
    /// is taken out.
    pub fn sync(&mut self) -> Report {
        let mut report = Report::default();
        let agents: Vec<Agent> = self.detected().cloned().collect();
        for a in &agents {
            if let Some(f) = &a.mcp {
                self.sync_mcp(a, f, &mut report);
            }
            if let Some(dir) = &a.skills {
                self.sync_skills(a, dir, &mut report);
            }
        }
        self.lib
            .applied
            .retain(|_, a| !a.mcp.is_empty() || !a.skills.is_empty());
        report
    }

    fn sync_mcp(&mut self, a: &Agent, f: &crate::agents::McpFile, report: &mut Report) {
        let want: Vec<&Server> = self
            .lib
            .mcp
            .iter()
            .filter(|e| self.server_state(e, a) == State::On)
            .map(|e| &e.server)
            .collect();
        let names: BTreeSet<String> = want.iter().map(|s| s.name.clone()).collect();
        let applied = self
            .lib
            .applied
            .get(a.id)
            .map(|x| x.mcp.clone())
            .unwrap_or_default();
        let remove: Vec<String> = applied.difference(&names).cloned().collect();
        self.backup(a.id, &f.path);
        match mcp::write(f, &want, &remove) {
            Ok(changed) => {
                report.changed += changed as usize;
                self.lib.applied.entry(a.id.to_string()).or_default().mcp = names;
            }
            Err(e) => report.issues.push(format!("{}: {e:#}", a.name)),
        }
    }

    fn sync_skills(&mut self, a: &Agent, dir: &Path, report: &mut Report) {
        let want: Vec<String> = self
            .lib
            .skills
            .iter()
            .filter(|e| self.skill_state(e, a) == State::On)
            .map(|e| e.name.clone())
            .collect();
        let applied = self
            .lib
            .applied
            .get(a.id)
            .map(|x| x.skills.clone())
            .unwrap_or_default();
        let mut have = BTreeSet::new();
        for name in &want {
            let target = self.skill_dir(name);
            let at = dir.join(name);
            if skills::links_to(&at, &target) {
                have.insert(name.clone());
                continue;
            }
            if fs::symlink_metadata(&at).is_ok() {
                // a link siu made that points somewhere stale is its own to fix
                if applied.contains(name) && fs::read_link(&at).is_ok() {
                    let _ = fs::remove_file(&at);
                } else {
                    report.issues.push(format!(
                        "{}: already has its own skill called {name}",
                        a.name
                    ));
                    continue;
                }
            }
            match skills::link(&target, &at) {
                Ok(()) => {
                    report.changed += 1;
                    have.insert(name.clone());
                }
                Err(e) => report.issues.push(format!("{}: {e:#}", a.name)),
            }
        }
        for name in applied.difference(&have) {
            if want.contains(name) {
                continue;
            }
            let at = dir.join(name);
            let ours = fs::read_link(&at)
                .is_ok_and(|t| t.starts_with(self.skills_dir()) || t == self.skill_dir(name));
            if ours {
                match fs::remove_file(&at) {
                    Ok(()) => report.changed += 1,
                    Err(e) => report.issues.push(format!("{}: {e}", a.name)),
                }
            }
        }
        self.lib.applied.entry(a.id.to_string()).or_default().skills = have;
    }

    /// Keeps the first version of an agent's file siu ever touched.
    fn backup(&self, agent: &str, path: &Path) {
        let Some(name) = path.file_name() else { return };
        let to = self.root.join("backups").join(agent).join(name);
        if path.is_file() && !to.exists() {
            let _ = fs::create_dir_all(to.parent().unwrap());
            let _ = fs::copy(path, &to);
        }
    }

    /// Removes skill folders in the library that no entry has any more.
    pub fn prune(&self) {
        let Ok(rd) = fs::read_dir(self.skills_dir()) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if self.skill(&name).is_none() && e.path().is_dir() {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::Transport;
    use serde_json::Value;

    struct Fixture {
        _d: tempfile::TempDir,
        home: PathBuf,
        store: Store,
    }

    /// A home with Claude Code, Codex and Gemini CLI installed.
    fn fixture() -> Fixture {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        for dir in [".claude", ".codex", ".gemini"] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        fs::write(home.join(".claude.json"), r#"{"numStartups":3,"mcpServers":{"mine":{"type":"stdio","command":"me","args":[],"env":{}}}}"#).unwrap();
        fs::write(home.join(".codex/config.toml"), "model = \"o3\"\n").unwrap();
        let store = Store::open(&Env::rooted(&home), home.join(".siu")).unwrap();
        Fixture { _d: d, home, store }
    }

    fn ctx7() -> Server {
        Server {
            name: "ctx7".into(),
            transport: Transport::Http,
            url: "https://mcp.context7.com/mcp".into(),
            ..Default::default()
        }
    }

    fn claude_servers(home: &Path) -> Value {
        let v: Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        v["mcpServers"].clone()
    }

    #[test]
    fn only_installed_agents_are_offered() {
        let f = fixture();
        let ids: Vec<_> = f.store.mcp_agents().iter().map(|a| a.id).collect();
        assert_eq!(ids, ["claude", "codex", "gemini"]);
    }

    #[test]
    fn server_lifecycle_touches_only_its_own_entries() {
        let mut f = fixture();
        let agents = f.store.default_server_agents(&ctx7());
        f.store.add_server(ctx7(), agents, None).unwrap();
        let r = f.store.commit().unwrap();
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert_eq!(
            claude_servers(&f.home)["ctx7"]["url"],
            "https://mcp.context7.com/mcp"
        );
        assert_eq!(claude_servers(&f.home)["mine"]["command"], "me");
        let codex = fs::read_to_string(f.home.join(".codex/config.toml")).unwrap();
        assert!(
            codex.contains("[mcp_servers.ctx7]") && codex.starts_with("model = \"o3\""),
            "{codex}"
        );
        let gemini = fs::read_to_string(f.home.join(".gemini/settings.json")).unwrap();
        assert!(gemini.contains("httpUrl"));

        // off for one agent
        assert!(!f.store.toggle_server_agent("ctx7", "codex").unwrap());
        f.store.commit().unwrap();
        assert!(
            !fs::read_to_string(f.home.join(".codex/config.toml"))
                .unwrap()
                .contains("ctx7")
        );

        // paused: picked agents kept, written nowhere
        assert!(!f.store.toggle_server_enabled("ctx7").unwrap());
        f.store.commit().unwrap();
        assert!(claude_servers(&f.home).get("ctx7").is_none());
        assert_eq!(
            f.store.server_state(
                f.store.server("ctx7").unwrap(),
                f.store.agent("claude").unwrap()
            ),
            State::Paused
        );
        f.store.toggle_server_enabled("ctx7").unwrap();
        f.store.commit().unwrap();
        assert!(claude_servers(&f.home).get("ctx7").is_some());

        // edit and rename: the old name goes, the new one comes
        let mut s = ctx7();
        s.name = "context7".into();
        f.store.edit_server("ctx7", s).unwrap();
        f.store.commit().unwrap();
        assert!(claude_servers(&f.home).get("ctx7").is_none());
        assert!(claude_servers(&f.home).get("context7").is_some());

        f.store.remove_server("context7").unwrap();
        f.store.commit().unwrap();
        let servers = claude_servers(&f.home);
        assert_eq!(
            servers.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["mine"]
        );
        assert!(f.home.join(".siu/backups/claude/.claude.json").is_file());
    }

    #[test]
    fn found_servers_can_be_imported() {
        let mut f = fixture();
        let found = f.store.found_servers();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agents, ["claude"]);
        f.store.import_server(&found[0]).unwrap();
        f.store.commit().unwrap();
        assert!(f.store.found_servers().is_empty());
        // now it's siu's: switching it off for Claude takes it out
        f.store.toggle_server_agent("mine", "claude").unwrap();
        f.store.commit().unwrap();
        assert!(claude_servers(&f.home).get("mine").is_none());
    }

    #[test]
    fn existing_desktop_is_stdio_only() {
        let mut f = fixture();
        fs::create_dir_all(f.home.join(".config/Claude")).unwrap();
        f.store = Store::open(&Env::rooted(&f.home), f.home.join(".siu")).unwrap();
        let agents = f.store.default_server_agents(&ctx7());
        assert!(!agents.contains("claude-desktop"));
        let desktop = f.store.agent("claude-desktop").unwrap().clone();
        f.store.add_server(ctx7(), agents, None).unwrap();
        let e = f.store.server("ctx7").unwrap();
        assert!(matches!(
            f.store.server_state(e, &desktop),
            State::Unsupported(_)
        ));
    }

    fn skill_tree(root: &Path, names: &[&str]) {
        for n in names {
            let d = root.join("skills").join(n);
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("SKILL.md"),
                format!("---\nname: {n}\ndescription: about {n}\n---\nv1"),
            )
            .unwrap();
        }
    }

    #[test]
    fn skill_lifecycle_links_and_unlinks() {
        let mut f = fixture();
        let tree = f.home.join("tree");
        skill_tree(&tree, &["pdf", "docx"]);
        let found = skills::find(&tree);
        let agents = f.store.default_skill_agents();
        let names = f
            .store
            .install_skills(
                &found,
                |x| {
                    Some(Source::Github {
                        repo: "a/b".into(),
                        reference: "HEAD".into(),
                        path: x.path.clone(),
                    })
                },
                &agents,
            )
            .unwrap();
        assert_eq!(names, ["docx", "pdf"]);
        let r = f.store.commit().unwrap();
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        let link = f.home.join(".claude/skills/pdf");
        assert!(skills::links_to(&link, &f.store.skill_dir("pdf")));
        assert!(
            fs::read_to_string(link.join("SKILL.md"))
                .unwrap()
                .contains("v1")
        );

        // updating from a newer tree keeps the links working
        fs::write(
            tree.join("skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: new\n---\nv2",
        )
        .unwrap();
        assert!(matches!(
            f.store.update_skill_from("pdf", &tree).unwrap(),
            UpdateOutcome::Updated
        ));
        assert!(matches!(
            f.store.update_skill_from("pdf", &tree).unwrap(),
            UpdateOutcome::UpToDate
        ));
        assert!(
            fs::read_to_string(link.join("SKILL.md"))
                .unwrap()
                .contains("v2")
        );
        assert_eq!(f.store.skill("pdf").unwrap().description, "new");

        f.store.toggle_skill_agent("pdf", "claude").unwrap();
        f.store.commit().unwrap();
        assert!(fs::symlink_metadata(&link).is_err());
        assert!(f.home.join(".codex/skills/pdf").exists());

        f.store.remove_skill("pdf").unwrap();
        f.store.commit().unwrap();
        f.store.prune();
        assert!(fs::symlink_metadata(f.home.join(".codex/skills/pdf")).is_err());
        assert!(!f.store.skill_dir("pdf").exists());
        assert!(f.store.skill_dir("docx").exists());
    }

    #[test]
    fn a_users_own_skill_is_never_replaced_until_imported() {
        let mut f = fixture();
        skill_tree(&f.home.join(".claude"), &["pdf"]); // ~/.claude/skills/pdf, the user's own
        let found = f.store.found_skills();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].description, "about pdf");

        let tree = f.home.join("tree");
        skill_tree(&tree, &["pdf"]);
        let agents = f.store.default_skill_agents();
        f.store
            .install_skills(&skills::find(&tree), |_| None, &agents)
            .unwrap();
        let r = f.store.commit().unwrap();
        assert_eq!(r.issues.len(), 1, "{:?}", r.issues);
        assert!(f.home.join(".claude/skills/pdf").is_dir());
        assert!(fs::read_link(f.home.join(".claude/skills/pdf")).is_err());
        assert!(skills::links_to(
            &f.home.join(".codex/skills/pdf"),
            &f.store.skill_dir("pdf")
        ));
    }

    #[test]
    fn importing_a_skill_backs_up_and_links() {
        let mut f = fixture();
        skill_tree(&f.home.join(".claude"), &["mine"]);
        let found = f.store.found_skills();
        f.store.import_skill(&found[0]).unwrap();
        let r = f.store.commit().unwrap();
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert!(skills::links_to(
            &f.home.join(".claude/skills/mine"),
            &f.store.skill_dir("mine")
        ));
        assert!(f.store.found_skills().is_empty());
        let backups = fs::read_dir(f.home.join(".siu/backups/skills/claude"))
            .unwrap()
            .count();
        assert_eq!(backups, 1);
        assert_eq!(
            f.store.skill("mine").unwrap().agents,
            BTreeSet::from(["claude".to_string()])
        );
    }

    #[test]
    fn library_survives_a_reopen() {
        let mut f = fixture();
        f.store
            .add_server(
                ctx7(),
                BTreeSet::from(["claude".into()]),
                Some("featured".into()),
            )
            .unwrap();
        f.store.commit().unwrap();
        let again = Store::open(&Env::rooted(&f.home), f.home.join(".siu")).unwrap();
        assert_eq!(again.lib.mcp, f.store.lib.mcp);
        assert_eq!(
            again.lib.applied["claude"].mcp,
            BTreeSet::from(["ctx7".to_string()])
        );
    }
}
