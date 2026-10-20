//! The whole setup in one JSON file, to move to another machine or restore:
//! MCP servers, skills with their files, and siu-gui's proxy settings and
//! theme. The format is siu-gui's, so a file exported by either opens in
//! the other. Only the desired state travels; what siu wrote into each
//! agent is rebuilt on the destination.
//!
//! `config.json` in the library folder is a standing restore request: siu
//! applies it at launch and on reload, once per distinct content, and
//! remembers what it applied in `config.applied.json` (shared with siu-gui).

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::library::{self, McpEntry, Report, SkillEntry, Source, Store};
use crate::{mcp, skills};

const FORMAT: &str = "siu-gui-backup";
const MAX_BACKUP_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILES: usize = 20_000;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Backup {
    format: String,
    version: u32,
    exported_at: u64,
    source_home: String,
    source_root: String,
    platform: String,
    mcp: Vec<McpEntry>,
    skills: Vec<SavedSkill>,
    proxy: Proxy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedSkill {
    entry: SkillEntry,
    files: Vec<SavedFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedFile {
    path: String,
    /// Binary-safe, including scripts and non-text resources.
    data: String,
    executable: bool,
}

/// siu-gui's download proxy (`proxy.json`). siu itself goes by the
/// environment's proxy variables; it only carries this along.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Proxy {
    #[serde(default)]
    mode: ProxyMode,
    #[serde(default)]
    url: String,
    #[serde(default)]
    no_proxy: String,
}

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ProxyMode {
    #[default]
    System,
    Custom,
    Direct,
}

impl Proxy {
    fn load(root: &Path) -> Result<Proxy> {
        let path = root.join("proxy.json");
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Proxy>(&bytes)
                .ok()
                .and_then(|p| p.validated().ok())
                .with_context(|| format!("{} isn't valid", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Proxy::default()),
            Err(e) => Err(e).with_context(|| format!("{}", path.display())),
        }
    }

    fn validated(mut self) -> Result<Proxy> {
        self.url = self.url.trim().to_string();
        self.no_proxy = self.no_proxy.trim().to_string();
        ensure!(
            self.url.len() <= 4096 && self.no_proxy.len() <= 8192,
            "the proxy settings are too long"
        );
        if self.mode == ProxyMode::Custom {
            let (scheme, rest) = self.url.split_once("://").unwrap_or_default();
            let host = rest.strip_suffix('/').unwrap_or(rest);
            ensure!(
                matches!(scheme, "http" | "https" | "socks5" | "socks5h")
                    && !host.is_empty()
                    && !host.contains(['/', '?', '#']),
                "the proxy address isn't an http, https or socks5 one"
            );
        }
        Ok(self)
    }
}

/// What `config.json` was last applied as.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppliedConfig {
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme: Option<String>,
}

pub struct Exported {
    pub mcp: usize,
    pub skills: usize,
}

pub struct Preview {
    pub digest: String,
    pub exported_at: u64,
    pub mcp: usize,
    pub skills: usize,
    /// What won't work as it did on the machine it came from.
    pub issues: Vec<String>,
}

pub struct Restored {
    pub report: Report,
    /// The setup as it was before, saved first.
    pub saved: PathBuf,
    /// siu-gui's theme: system, light or dark.
    pub theme: Option<String>,
}

fn valid_theme(theme: Option<&str>) -> Result<()> {
    ensure!(
        theme.is_none_or(|t| matches!(t, "system" | "light" | "dark")),
        "the backup's theme isn't system, light or dark"
    );
    Ok(())
}

/// Checked the same on every OS: a file made on Unix must not become an
/// absolute path, a stream or a device name when restored on Windows.
fn relative_path(path: &str) -> Result<PathBuf> {
    ensure!(
        !path.is_empty() && path.len() <= 4096,
        "the backup has an invalid file path"
    );
    let mut out = PathBuf::new();
    for part in path.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        ensure!(
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part
                    .chars()
                    .any(|c| c.is_control() || "\\:<>\"|?*".contains(c))
                && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9')),
            "the backup has a file path siu can't restore: {path}"
        );
        out.push(part);
    }
    ensure!(
        out.components().count() <= 64,
        "the backup's folders nest too deep"
    );
    Ok(out)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "{} is too big", path.display());
    Ok(bytes)
}

fn not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

fn collect_files(
    root: &Path,
    dir: &Path,
    files: &mut Vec<SavedFile>,
    count: &mut usize,
    size: &mut usize,
    depth: usize,
) -> Result<()> {
    ensure!(depth <= 64, "{} nests too deep", root.display());
    let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(root)?
            .to_str()
            .with_context(|| format!("{} isn't UTF-8", path.display()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        relative_path(&rel)?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(root, &path, files, count, size, depth + 1)?;
        } else {
            if kind.is_symlink() {
                let target = fs::canonicalize(&path)?;
                ensure!(
                    target.starts_with(root) && target.is_file(),
                    "{} links outside its skill or to a folder",
                    path.display()
                );
            } else {
                ensure!(kind.is_file(), "{} isn't a file or folder", path.display());
            }
            *count += 1;
            ensure!(
                *count <= MAX_FILES,
                "the skills have over {MAX_FILES} files"
            );
            let bytes = read_bounded(&path, (MAX_CONTENT_BYTES - *size) as u64)?;
            *size += bytes.len();
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                fs::metadata(&path)?.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            files.push(SavedFile {
                path: rel,
                data: STANDARD.encode(bytes),
                executable,
            });
        }
    }
    Ok(())
}

fn capture(store: &Store, home: &Path) -> Result<Backup> {
    let mut saved = Vec::new();
    let (mut count, mut size) = (0, 0);
    for entry in &store.lib.skills {
        mcp::valid_name(&entry.name)?;
        relative_path(&entry.name)?;
        let root = fs::canonicalize(store.skill_dir(&entry.name))
            .with_context(|| format!("couldn't read skill {}", entry.name))?;
        let mut files = Vec::new();
        collect_files(&root, &root, &mut files, &mut count, &mut size, 0)?;
        ensure!(
            files.iter().any(|f| f.path == "SKILL.md"),
            "skill {} has no SKILL.md",
            entry.name
        );
        saved.push(SavedSkill {
            entry: entry.clone(),
            files,
        });
    }
    Ok(Backup {
        format: FORMAT.into(),
        version: 1,
        exported_at: library::now(),
        source_home: home.to_str().context("the home folder isn't UTF-8")?.into(),
        source_root: store
            .root
            .to_str()
            .context("the library folder isn't UTF-8")?
            .into(),
        platform: std::env::consts::OS.into(),
        mcp: store.lib.mcp.clone(),
        skills: saved,
        proxy: Proxy::load(&store.root)?,
        theme: None,
    })
}

fn encode(backup: &Backup) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(backup)?;
    ensure!(
        bytes.len() as u64 <= MAX_BACKUP_BYTES,
        "the backup would be over 128 MiB"
    );
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn validate(backup: &Backup) -> Result<()> {
    ensure!(
        backup.format == FORMAT && backup.version == 1,
        "not a siu backup, or one from a newer version"
    );
    ensure!(
        !backup.source_home.is_empty() && !backup.source_root.is_empty(),
        "the backup doesn't say which folders it came from"
    );
    valid_theme(backup.theme.as_deref())?;
    backup.proxy.clone().validated()?;
    let mut names = BTreeSet::new();
    for entry in &backup.mcp {
        entry
            .server
            .validate()
            .with_context(|| format!("server {} in the backup", entry.server.name))?;
        ensure!(
            names.insert(entry.server.name.to_ascii_lowercase()),
            "the backup has two servers called {}",
            entry.server.name
        );
    }
    names.clear();
    let (mut count, mut size) = (0, 0usize);
    for saved in &backup.skills {
        mcp::valid_name(&saved.entry.name)?;
        relative_path(&saved.entry.name)?;
        ensure!(
            names.insert(saved.entry.name.to_ascii_lowercase()),
            "the backup has two skills called {}",
            saved.entry.name
        );
        let mut paths = BTreeSet::new();
        for file in &saved.files {
            relative_path(&file.path)?;
            ensure!(
                paths.insert(file.path.to_lowercase()),
                "skill {} has {} twice",
                saved.entry.name,
                file.path
            );
            count += 1;
            ensure!(count <= MAX_FILES, "the skills have over {MAX_FILES} files");
            size = size
                .checked_add(
                    STANDARD
                        .decode(&file.data)
                        .context("a file in the backup isn't valid base64")?
                        .len(),
                )
                .context("the backup's files are too big")?;
            ensure!(
                size <= MAX_CONTENT_BYTES,
                "the skills' files are over 64 MiB"
            );
        }
        ensure!(
            saved.files.iter().any(|f| f.path == "SKILL.md"),
            "skill {} has no SKILL.md",
            saved.entry.name
        );
        for file in &saved.files {
            let lower = file.path.to_lowercase();
            for (i, _) in lower.match_indices('/') {
                ensure!(
                    !paths.contains(&lower[..i]),
                    "skill {} has {} as both a file and a folder",
                    saved.entry.name,
                    &file.path[..i]
                );
            }
        }
    }
    Ok(())
}

fn load(path: &Path) -> Result<(Backup, String)> {
    let bytes =
        read_bounded(path, MAX_BACKUP_BYTES).with_context(|| format!("{}", path.display()))?;
    let backup: Backup = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("{} isn't a siu backup", path.display()))?;
    validate(&backup)?;
    Ok((backup, digest(&bytes)))
}

/// Moves a path under the old home or library folder to the same place
/// under this machine's. Only whole leading folders are replaced, never
/// text inside tokens or URLs; the library wins when it's outside home.
fn rebase(value: &str, backup: &Backup, store: &Store, home: &Path) -> String {
    for (old, new) in [
        (backup.source_root.as_str(), store.root.as_path()),
        (backup.source_home.as_str(), home),
    ] {
        let old = old.trim_end_matches(['/', '\\']);
        if let Some(tail) = value.strip_prefix(old)
            && (tail.is_empty() || tail.starts_with(['/', '\\']))
        {
            let mut result = new.to_path_buf();
            for part in tail.split(['/', '\\']).filter(|p| !p.is_empty()) {
                result.push(part);
            }
            return result.to_string_lossy().into();
        }
    }
    if let Some((flag, path)) = value.split_once('=')
        && flag.starts_with('-')
    {
        return format!("{flag}={}", rebase(path, backup, store, home));
    }
    value.into()
}

fn restore_entries(
    backup: &Backup,
    store: &Store,
    home: &Path,
) -> (Vec<McpEntry>, Vec<SkillEntry>) {
    let mut servers = backup.mcp.clone();
    for entry in &mut servers {
        entry.server.command = rebase(&entry.server.command, backup, store, home);
        for arg in &mut entry.server.args {
            *arg = rebase(arg, backup, store, home);
        }
        for value in entry.server.env.values_mut() {
            *value = rebase(value, backup, store, home);
        }
    }
    let mut entries: Vec<_> = backup.skills.iter().map(|s| s.entry.clone()).collect();
    for entry in &mut entries {
        if let Some(Source::Local { dir }) = &mut entry.source {
            *dir = PathBuf::from(rebase(&dir.to_string_lossy(), backup, store, home));
        }
    }
    (servers, entries)
}

/// Refuses to take over a server of the same name that an agent has and
/// siu didn't write there; it has to be imported first.
fn check_mcp_collision(store: &Store, name: &str, agents: &BTreeSet<String>) -> Result<()> {
    for id in agents {
        let Some(agent) = store.agent(id) else {
            continue;
        };
        let Some(file) = &agent.mcp else { continue };
        let ours = store
            .lib
            .applied
            .get(id)
            .is_some_and(|a| a.mcp.contains(name));
        // an unreadable file says so itself when it's written
        if !ours && mcp::read(file).is_ok_and(|have| have.contains_key(name)) {
            bail!(
                "{} already has its own server called {name}; import it first",
                agent.name
            );
        }
    }
    Ok(())
}

fn check_targets(
    store: &Store,
    servers: &[McpEntry],
    entries: &[SkillEntry],
) -> Result<Vec<String>> {
    let mut issues = BTreeSet::new();
    for (kind, name, ids, server) in servers
        .iter()
        .map(|e| ("server", e.server.name.as_str(), &e.agents, Some(&e.server)))
        .chain(
            entries
                .iter()
                .map(|e| ("skill", e.name.as_str(), &e.agents, None)),
        )
    {
        for id in ids {
            let Some(agent) = store.agent(id) else {
                bail!("the backup names an agent siu doesn't know: {id}");
            };
            if let Some(server) = server {
                let Some(file) = agent.mcp.as_ref().filter(|_| id != "agents") else {
                    bail!(
                        "the backup gives server {name} to {}, which has no MCP config",
                        agent.name
                    );
                };
                if let Some(why) = mcp::unsupported(file.format, server) {
                    issues.insert(format!(
                        "{kind} {name}: {} can't use it ({why}); kept picked",
                        agent.name
                    ));
                }
            } else {
                let dir = agent.skills.as_ref().with_context(|| {
                    format!(
                        "the backup gives skill {name} to {}, which has no skills folder",
                        agent.name
                    )
                })?;
                let at = dir.join(name);
                let owned_link = store
                    .lib
                    .applied
                    .get(id)
                    .is_some_and(|a| a.skills.contains(name))
                    && fs::read_link(&at).is_ok();
                if fs::symlink_metadata(&at).is_ok()
                    && !skills::links_to(&at, &store.skill_dir(name))
                    && !owned_link
                {
                    issues.insert(format!(
                        "{kind} {name}: {} already has its own; it stays until you import or move it",
                        agent.name
                    ));
                }
            }
            if !agent.detected() && id != "agents" {
                issues.insert(format!(
                    "{kind} {name}: {} isn't installed here; kept picked for when it is",
                    agent.name
                ));
            }
        }
        if let Some(server) = server {
            check_mcp_collision(store, name, ids)?;
            if Path::new(&server.command).is_absolute() && !Path::new(&server.command).exists() {
                issues.insert(format!(
                    "{kind} {name}: {} isn't on this machine; edit its command",
                    server.command
                ));
            }
        }
    }
    Ok(issues.into_iter().collect())
}

/// Writes a file only the user can read: it may hold credentials.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    mcp::write_atomic(path, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// A path with its links resolved, so far as it exists: a file not made
/// yet compares by its folder.
fn real(path: &Path) -> PathBuf {
    if let Ok(p) = fs::canonicalize(path) {
        return p;
    }
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => real(dir).join(name),
        _ => path.to_path_buf(),
    }
}

pub fn export(store: &Store, home: &Path, path: &Path) -> Result<Exported> {
    ensure!(path.is_absolute(), "give the backup an absolute path");
    ensure!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json")),
        "the backup's name has to end in .json"
    );
    // never over the library's own files or an agent's config
    let parent = path.parent().context("not a file path")?;
    ensure!(parent.is_dir(), "there's no folder {}", parent.display());
    let target = real(path);
    let root = real(&store.root);
    ensure!(
        !target.starts_with(&root) || target == root.join("config.json"),
        "inside the library only config.json can be written; pick another place"
    );
    for agent in &store.agents {
        if let Some(file) = &agent.mcp {
            ensure!(
                target != real(&file.path),
                "that's {}'s MCP config",
                agent.name
            );
        }
        if let Some(dir) = &agent.skills {
            ensure!(
                !target.starts_with(real(dir)),
                "that's inside {}'s skills folder",
                agent.name
            );
        }
    }
    let backup = capture(store, home)?;
    validate(&backup)?;
    let mut file = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
    file.write_all(&encode(&backup)?)?;
    file.as_file().sync_all()?;
    file.persist(&target)?;
    Ok(Exported {
        mcp: backup.mcp.len(),
        skills: backup.skills.len(),
    })
}

pub fn preview(store: &Store, home: &Path, path: &Path) -> Result<Preview> {
    let (backup, digest) = load(path)?;
    let (servers, entries) = restore_entries(&backup, store, home);
    let issues = check_targets(store, &servers, &entries)?;
    Ok(Preview {
        digest,
        exported_at: backup.exported_at,
        mcp: servers.len(),
        skills: entries.len(),
        issues,
    })
}

fn restore_file(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    match bytes {
        Some(bytes) => mcp::write_atomic(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        },
    }
}

fn existing_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Replaces every server and skill siu manages with the backup's, then
/// writes them into the agents. `expected` is the digest the preview
/// showed, so a file changed since isn't restored unseen. The setup as it
/// was is saved to `backups/` first.
pub fn import(store: &mut Store, home: &Path, path: &Path, expected: &str) -> Result<Restored> {
    let (backup, actual) = load(path)?;
    ensure!(
        actual == expected,
        "{} changed since it was opened; open it again",
        path.display()
    );
    let (servers, mut entries) = restore_entries(&backup, store, home);
    let warnings = check_targets(store, &servers, &entries)?;
    fs::create_dir_all(&store.root)?;
    let work = tempfile::Builder::new()
        .prefix(".restore-")
        .tempdir_in(&store.root)?;
    let staged = work.path().join("new");
    for (saved, entry) in backup.skills.iter().zip(&mut entries) {
        let dir = staged.join(&entry.name);
        for file in &saved.files {
            let to = dir.join(relative_path(&file.path)?);
            fs::create_dir_all(to.parent().unwrap())?;
            fs::write(&to, STANDARD.decode(&file.data)?)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &to,
                    fs::Permissions::from_mode(if file.executable { 0o700 } else { 0o600 }),
                )?;
            }
        }
        entry.hash = skills::hash_dir(&dir);
    }
    let previous_library = existing_bytes(&store.root.join("library.json"))?;
    let previous_proxy = existing_bytes(&store.root.join("proxy.json"))?;
    let previous_backup = capture(store, home)?;
    validate(&previous_backup)?;
    let bytes = encode(&previous_backup)?;
    let backups = store.root.join("backups");
    fs::create_dir_all(&backups)?;
    let mut safety = tempfile::Builder::new()
        .prefix(&format!("config-{}-", library::now()))
        .suffix(".json")
        .tempfile_in(backups)?;
    safety.write_all(&bytes)?;
    safety.as_file().sync_all()?;
    let (_, saved) = safety.keep()?;

    // Skill folders taken out stay until sync has removed every link to
    // them. Only incoming names are swapped, the old copy kept until saved.
    let skills_dir = store.skills_dir();
    if let Ok(meta) = fs::symlink_metadata(&skills_dir) {
        ensure!(
            !meta.file_type().is_symlink() && meta.is_dir(),
            "{} has to be a real folder",
            skills_dir.display()
        );
    }
    fs::create_dir_all(&skills_dir)?;
    let old = work.path().join("old");
    fs::create_dir_all(&old)?;
    let mut swapped = Vec::new();
    let previous = store.lib.clone();
    let persist = (|| -> Result<()> {
        for entry in &entries {
            let target = store.skill_dir(&entry.name);
            if fs::symlink_metadata(&target).is_ok() {
                ensure!(
                    target.is_dir() && !fs::symlink_metadata(&target)?.file_type().is_symlink(),
                    "{} isn't a skill folder",
                    target.display()
                );
                fs::rename(&target, old.join(&entry.name))?;
            }
            swapped.push(entry.name.clone());
            fs::rename(staged.join(&entry.name), target)?;
        }
        store.lib.mcp = servers;
        store.lib.skills = entries;
        let mut proxy = serde_json::to_vec_pretty(&backup.proxy.clone().validated()?)?;
        proxy.push(b'\n');
        write_private(&store.root.join("proxy.json"), &proxy)?;
        store.save()?;
        Ok(())
    })();
    if let Err(error) = persist {
        store.lib = previous;
        let mut rollback_errors = Vec::new();
        for name in swapped.iter().rev() {
            let target = store.skill_dir(name);
            if target.exists()
                && let Err(e) = fs::remove_dir_all(&target)
            {
                rollback_errors.push(e.to_string());
            }
            if old.join(name).exists()
                && let Err(e) = fs::rename(old.join(name), target)
            {
                rollback_errors.push(e.to_string());
            }
        }
        for (name, bytes) in [
            ("library.json", previous_library.as_deref()),
            ("proxy.json", previous_proxy.as_deref()),
        ] {
            if let Err(e) = restore_file(&store.root.join(name), bytes) {
                rollback_errors.push(e.to_string());
            }
        }
        if !rollback_errors.is_empty() {
            // keep what's needed to undo by hand
            let undo = work.keep();
            bail!(
                "restore failed: {error:#}; undoing it failed too: {}; the previous setup is in {} and the half-restored files in {}",
                rollback_errors.join("; "),
                saved.display(),
                undo.display()
            );
        }
        bail!(
            "restore failed and was undone: {error:#}; the previous setup is also in {}",
            saved.display()
        );
    }
    // the shared skills folder may be picked before any agent made it
    let mut setup_issues = Vec::new();
    if store
        .lib
        .skills
        .iter()
        .any(|e| e.enabled && e.agents.contains("agents"))
        && let Some(dir) = store.agent("agents").and_then(|a| a.skills.as_ref())
        && let Err(error) = fs::create_dir_all(dir)
    {
        setup_issues.push(format!("couldn't make {}: {error}", dir.display()));
    }
    let mut report = store.sync();
    report.issues.extend(setup_issues);
    if let Err(error) = store.save() {
        report.issues.push(format!(
            "couldn't save what was written: {error:#}; reload to retry"
        ));
    }
    if report.issues.is_empty()
        && !store
            .lib
            .applied
            .values()
            .any(|a| a.skills.iter().any(|name| store.skill(name).is_none()))
    {
        store.prune();
    }
    // config.json counts as applied once the library holds it; writing
    // into the agents can be retried on its own
    if let (Ok(selected), Ok(default)) = (
        fs::canonicalize(path),
        fs::canonicalize(store.root.join("config.json")),
    ) && selected == default
    {
        let applied = AppliedConfig {
            digest: actual,
            theme: backup.theme.clone(),
        };
        if let Err(error) = mcp::write_atomic(
            &store.root.join("config.applied.json"),
            &serde_json::to_vec(&applied)?,
        ) {
            report
                .issues
                .push(format!("couldn't note config.json as applied: {error:#}"));
        }
    }
    report.issues.extend(warnings);
    report.changed += backup.mcp.len() + backup.skills.len();
    Ok(Restored {
        report,
        saved,
        theme: backup.theme,
    })
}

fn applied_digest(root: &Path) -> Result<Option<String>> {
    let path = root.join("config.applied.json");
    let bytes = match read_bounded(&path, 4096) {
        Ok(bytes) => bytes,
        Err(e) if not_found(&e) => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
    };
    let applied: AppliedConfig = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "{} isn't valid; move it away to apply config.json again",
            path.display()
        )
    })?;
    valid_theme(applied.theme.as_deref())?;
    Ok(Some(applied.digest))
}

/// Restores `config.json` from the library folder when it's there and
/// differs from what was last applied; None when there's nothing new.
pub fn apply_default(store: &mut Store, home: &Path) -> Result<Option<Restored>> {
    let path = store.root.join("config.json");
    let bytes = match read_bounded(&path, MAX_BACKUP_BYTES) {
        Ok(bytes) => bytes,
        Err(e) if not_found(&e) => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
    };
    let digest = digest(&bytes);
    if applied_digest(&store.root)?.as_deref() == Some(digest.as_str()) {
        return Ok(None);
    }
    import(store, home, &path, &digest).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Env;

    fn store(home: &Path) -> Store {
        Store::open(&Env::rooted(home), home.join(".siu")).unwrap()
    }

    fn skill(store: &mut Store, name: &str, content: &str) {
        let dir = store.skill_dir(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), content).unwrap();
        store.lib.skills.push(SkillEntry {
            name: name.into(),
            description: String::new(),
            source: None,
            agents: BTreeSet::new(),
            enabled: true,
            hash: skills::hash_dir(&dir),
            installed: 1,
            updated: 2,
        });
    }

    #[test]
    fn file_validation_rejects_traversal_and_cross_platform_collisions() {
        let temp = tempfile::tempdir().unwrap();
        let mut source = store(temp.path());
        skill(&mut source, "sample", "original");
        let mut backup = capture(&source, temp.path()).unwrap();
        for path in [
            "../escape",
            "/absolute",
            "C:/escape",
            "a\\b",
            "a/../b",
            "a//b",
            "NUL.txt",
            "a/COM1",
            "file:stream",
            "a/.",
            "a. ",
        ] {
            backup.skills[0].files[0].path = path.into();
            assert!(validate(&backup).is_err(), "accepted {path}");
        }
        backup.skills[0].files[0].path = "SKILL.md".into();
        backup.skills[0].files.push(SavedFile {
            path: "skill.MD".into(),
            data: STANDARD.encode(b"duplicate"),
            executable: false,
        });
        assert!(validate(&backup).is_err());
        backup.skills[0].files[1].path = "SKILL.md/child".into();
        assert!(validate(&backup).is_err());
        backup.skills[0].files[1].path = "valid.txt".into();
        backup.skills[0].files[1].data = "invalid base64!".into();
        assert!(validate(&backup).is_err());
        backup.skills[0].files.pop();
        assert!(validate(&backup).is_ok());
    }

    #[test]
    fn rebases_windows_and_unix_paths_without_substituting_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let destination = store(temp.path());
        let mut backup = capture(&destination, temp.path()).unwrap();
        for (home, root, file) in [
            ("/old/home", "/separate/library", "/old/home/tools/file"),
            (r"C:\Users\old", r"D:\siu", r"C:\Users\old\tools\file"),
        ] {
            backup.source_home = home.into();
            backup.source_root = root.into();
            assert_eq!(
                rebase(file, &backup, &destination, temp.path()),
                temp.path().join("tools/file").to_string_lossy()
            );
            assert_eq!(
                rebase(
                    &format!("{root}/skills/sample"),
                    &backup,
                    &destination,
                    temp.path()
                ),
                destination.skill_dir("sample").to_string_lossy()
            );
            assert_eq!(
                rebase(
                    &format!("--path={file}"),
                    &backup,
                    &destination,
                    temp.path()
                ),
                format!("--path={}", temp.path().join("tools/file").display())
            );
            let token = format!("Bearer token-{home}/secret");
            assert_eq!(rebase(&token, &backup, &destination, temp.path()), token);
            let similar = format!("{home}-other/file");
            assert_eq!(
                rebase(&similar, &backup, &destination, temp.path()),
                similar
            );
        }
    }

    #[test]
    fn persistence_failure_rolls_back_replaced_skill_files_and_config() {
        let from = tempfile::tempdir().unwrap();
        let to = tempfile::tempdir().unwrap();
        let mut source = store(from.path());
        skill(&mut source, "alpha", "incoming alpha");
        skill(&mut source, "omega", "incoming omega");
        let path = from.path().join("backup.json");
        export(&source, from.path(), &path).unwrap();
        let mut destination = store(to.path());
        skill(&mut destination, "alpha", "original alpha");
        destination.save().unwrap();
        fs::write(destination.root.join("proxy.json"), b"{}").unwrap();
        let library = fs::read(destination.root.join("library.json")).unwrap();
        // an orphan file stops the second swap after alpha was replaced
        fs::write(destination.skills_dir().join("omega"), b"orphan").unwrap();
        let inspected = preview(&destination, to.path(), &path).unwrap();
        let error = import(&mut destination, to.path(), &path, &inspected.digest)
            .err()
            .unwrap();
        assert!(error.to_string().contains("was undone"), "{error:#}");
        assert_eq!(
            fs::read(destination.skill_dir("alpha").join("SKILL.md")).unwrap(),
            b"original alpha"
        );
        assert_eq!(
            fs::read(destination.skills_dir().join("omega")).unwrap(),
            b"orphan"
        );
        assert_eq!(
            fs::read(destination.root.join("library.json")).unwrap(),
            library
        );
        assert_eq!(
            fs::read(destination.root.join("proxy.json")).unwrap(),
            b"{}"
        );
        assert_eq!(destination.lib.skills.len(), 1);
        assert!(
            fs::read_dir(destination.root.join("backups"))
                .unwrap()
                .any(|e| e.unwrap().path().extension().is_some_and(|x| x == "json"))
        );
    }

    #[test]
    fn a_changed_file_is_not_restored_unseen() {
        let temp = tempfile::tempdir().unwrap();
        let mut s = store(temp.path());
        let path = temp.path().join("backup.json");
        export(&s, temp.path(), &path).unwrap();
        let seen = preview(&s, temp.path(), &path).unwrap();
        let mut text = fs::read_to_string(&path).unwrap();
        text.push('\n');
        fs::write(&path, text).unwrap();
        let error = import(&mut s, temp.path(), &path, &seen.digest)
            .err()
            .unwrap();
        assert!(error.to_string().contains("changed since"), "{error:#}");
    }

    #[test]
    fn export_refuses_the_library_and_agent_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".claude/skills")).unwrap();
        let s = store(temp.path());
        fs::create_dir_all(&s.root).unwrap();
        for bad in [
            s.root.join("library.json"),
            temp.path().join(".claude.json"),
            temp.path().join(".claude/skills/x.json"),
            temp.path().join("relative.txt"),
        ] {
            assert!(export(&s, temp.path(), &bad).is_err(), "{}", bad.display());
        }
        assert!(export(&s, temp.path(), &s.root.join("config.json")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn exports_private_files_even_when_replacing_a_public_file() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let source = store(temp.path());
        let path = temp.path().join("backup.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        export(&source, temp.path(), &path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
