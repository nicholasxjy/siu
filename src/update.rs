//! `siu update`: siu itself from crates.io, and the library's MCP servers
//! and skills from where each came from.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::agents::Env;
use crate::github;
use crate::library::{Source, Store, UpdateOutcome};
use crate::mcp::Server;
use crate::net;

const CRATE: &str = env!("CARGO_PKG_NAME");
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// `siu update [--mcp] [--skill] [--all]`; no option updates siu itself.
pub fn run(args: &[String], env: &Env, root: PathBuf) -> Result<()> {
    let (mut siu, mut mcp, mut skills) = (false, false, false);
    for a in args {
        match a.as_str() {
            "--mcp" => mcp = true,
            "--skill" => skills = true,
            "--all" => (siu, mcp, skills) = (true, true, true),
            other => bail!("unknown option {other}; try --help"),
        }
    }
    if !mcp && !skills {
        return update_siu();
    }
    let mut out = std::io::stdout();
    let mut failed = 0;
    let mut store = Store::open(env, root)?;
    if mcp {
        failed += update_mcp(&mut store, &Net, &mut out)?;
    }
    if skills {
        if mcp {
            writeln!(out)?;
        }
        failed += update_skills(&mut store, &Net, &mut out)?;
    }
    if siu {
        writeln!(out)?;
        if let Err(e) = update_siu() {
            eprintln!("{e:#}");
            failed += 1;
        }
    }
    if failed > 0 {
        bail!("{failed} could not be updated");
    }
    Ok(())
}

// ---- siu ----------------------------------------------------------------

#[derive(Deserialize)]
struct Body {
    #[serde(rename = "crate")]
    krate: Krate,
}

#[derive(Deserialize)]
struct Krate {
    max_stable_version: Option<String>,
    max_version: String,
}

/// The newest stable version on crates.io.
pub fn latest() -> Result<String> {
    let body: Body = net::get_json(&format!("https://crates.io/api/v1/crates/{CRATE}"))
        .context("couldn't look up the latest version")?;
    Ok(body
        .krate
        .max_stable_version
        .unwrap_or(body.krate.max_version))
}

/// Whether `latest` is a later release than `current`.
fn newer(current: &str, latest: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    parts(latest) > parts(current)
}

fn update_siu() -> Result<()> {
    let latest = latest()?;
    if !newer(CURRENT, &latest) {
        println!("siu {CURRENT} is the latest version");
        return Ok(());
    }
    println!("updating siu {CURRENT} → {latest}");
    let status = Command::new("cargo")
        .args(["install", CRATE, "--locked", "--version", &latest])
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("siu {latest} installed");
            Ok(())
        }
        Ok(s) => bail!("cargo install failed ({s})"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "cargo not found; install Rust from https://rustup.rs, then run: cargo install {CRATE}"
            )
        }
        Err(e) => Err(e).context("couldn't run cargo"),
    }
}

// ---- what updates come from ---------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eco {
    Npm,
    Pypi,
}

/// Where updates are looked up and fetched; tests stand in for the network.
pub trait Remote {
    /// The newest release of a package.
    fn latest(&self, eco: Eco, name: &str) -> Result<String>;
    fn pull(&self, image: &str) -> Result<()>;
    fn download(&self, repo: &str, reference: &str) -> Result<(tempfile::TempDir, PathBuf)>;
}

struct Net;

#[derive(Deserialize)]
struct Release {
    version: String,
}

#[derive(Deserialize)]
struct Pypi {
    info: Release,
}

impl Remote for Net {
    fn latest(&self, eco: Eco, name: &str) -> Result<String> {
        Ok(match eco {
            Eco::Npm => {
                let url = format!(
                    "https://registry.npmjs.org/{}/latest",
                    name.replace('/', "%2f")
                );
                net::get_json::<Release>(&url)?.version
            }
            Eco::Pypi => {
                net::get_json::<Pypi>(&format!("https://pypi.org/pypi/{name}/json"))?
                    .info
                    .version
            }
        })
    }

    fn pull(&self, image: &str) -> Result<()> {
        let out = Command::new("docker")
            .args(["pull", "-q", image])
            .output()
            .context("couldn't run docker")?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("{}", err.trim());
        }
        Ok(())
    }

    fn download(&self, repo: &str, reference: &str) -> Result<(tempfile::TempDir, PathBuf)> {
        github::download(repo, reference)
    }
}

// ---- MCP servers --------------------------------------------------------

/// What updating a server means, read from what it runs.
#[derive(Debug, PartialEq)]
enum Plan {
    Remote,
    /// Not a package siu can look up.
    Unknown,
    /// A tag or range the runner resolves itself.
    Floating(String),
    /// `args[at]` names a package; a new version is written there as
    /// `{prefix}{base}{sep}{version}`.
    Package {
        eco: Eco,
        name: String,
        version: Option<String>,
        at: usize,
        prefix: &'static str,
        base: String,
        sep: &'static str,
    },
    Image(String),
}

fn plan(s: &Server) -> Plan {
    if s.remote() {
        return Plan::Remote;
    }
    let program = Path::new(&s.command)
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    match program {
        "npx" => npx_plan(&s.args),
        "uvx" => uvx_plan(&s.args),
        "docker" => docker_plan(&s.args),
        _ => Plan::Unknown,
    }
}

fn npx_plan(args: &[String]) -> Plan {
    for (i, a) in args.iter().enumerate() {
        match a.as_str() {
            "-p" | "--package" => {
                return match args.get(i + 1) {
                    Some(v) => npm_package(v, i + 1, ""),
                    None => Plan::Unknown,
                };
            }
            "-c" | "--call" => return Plan::Unknown,
            _ => {}
        }
        if let Some(v) = a.strip_prefix("--package=") {
            return npm_package(v, i, "--package=");
        }
        if a.starts_with("--call=") {
            return Plan::Unknown;
        }
        if !a.starts_with('-') {
            return npm_package(a, i, "");
        }
    }
    Plan::Unknown
}

fn npm_package(spec: &str, at: usize, prefix: &'static str) -> Plan {
    let split = match spec.strip_prefix('@') {
        Some(rest) => rest.find('@').map(|i| i + 1),
        None => spec.find('@'),
    };
    let (name, version) = match split {
        Some(i) => (&spec[..i], Some(&spec[i + 1..])),
        None => (spec, None),
    };
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with(['.', '_'])
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c))
    };
    let valid = match name.strip_prefix('@') {
        Some(scoped) => scoped
            .split_once('/')
            .is_some_and(|(scope, n)| part(scope) && part(n)),
        None => part(name),
    };
    if !valid {
        return Plan::Unknown;
    }
    match version {
        Some(v) if !npm_exact(v) => Plan::Floating(spec.into()),
        _ => Plan::Package {
            eco: Eco::Npm,
            name: name.into(),
            version: version.map(Into::into),
            at,
            prefix,
            base: name.into(),
            sep: "@",
        },
    }
}

/// `1.2.3` or `1.2.3-beta.1`, not a tag or a range.
fn npm_exact(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

fn uvx_plan(args: &[String]) -> Plan {
    // options of uvx that take the next argument as their value
    const VALUED: &[&str] = &[
        "-w",
        "--with",
        "--with-editable",
        "--with-requirements",
        "-p",
        "--python",
        "--index",
        "--index-url",
        "--extra-index-url",
        "--default-index",
        "-f",
        "--find-links",
        "-c",
        "--constraints",
        "--overrides",
        "--directory",
        "--project",
        "--config-file",
        "--cache-dir",
    ];
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--from" {
            return match args.get(i + 1) {
                Some(v) => pypi_package(v, i + 1, "", "=="),
                None => Plan::Unknown,
            };
        }
        if let Some(v) = a.strip_prefix("--from=") {
            return pypi_package(v, i, "--from=", "==");
        }
        if VALUED.contains(&a) {
            i += 2;
            continue;
        }
        if !a.starts_with('-') {
            return pypi_package(a, i, "", "@");
        }
        i += 1;
    }
    Plan::Unknown
}

fn pypi_package(spec: &str, at: usize, prefix: &'static str, sep: &'static str) -> Plan {
    let end = spec
        .find(|c: char| !(c.is_ascii_alphanumeric() || "-_.".contains(c)))
        .unwrap_or(spec.len());
    let name = &spec[..end];
    let mut rest = &spec[end..];
    if name.is_empty() {
        return Plan::Unknown;
    }
    if rest.starts_with('[') {
        match rest.find(']') {
            Some(i) => rest = &rest[i + 1..],
            None => return Plan::Unknown,
        }
    }
    let base = &spec[..spec.len() - rest.len()];
    let (version, sep) = if rest.is_empty() {
        (None, sep)
    } else if let Some(v) = rest.strip_prefix("==") {
        (Some(v), "==")
    } else if let Some(v) = rest.strip_prefix('@') {
        (Some(v), "@")
    } else if rest.starts_with(['<', '>', '~', '!', '=']) {
        return Plan::Floating(spec.into());
    } else {
        return Plan::Unknown;
    };
    if let Some(v) = version
        && !pypi_exact(v)
    {
        return Plan::Floating(spec.into());
    }
    Plan::Package {
        eco: Eco::Pypi,
        name: name.into(),
        version: version.map(Into::into),
        at,
        prefix,
        base: base.into(),
        sep,
    }
}

/// `1.2`, `2026.10.10`, `1.0.post1`, not `latest`, `1.*` or a range.
fn pypi_exact(v: &str) -> bool {
    v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-+!".contains(c))
}

fn docker_plan(args: &[String]) -> Plan {
    // options of docker run that take no value
    const BARE: &[&str] = &[
        "-i",
        "-t",
        "-it",
        "-ti",
        "-d",
        "-P",
        "-q",
        "--rm",
        "--init",
        "--interactive",
        "--tty",
        "--detach",
        "--privileged",
        "--publish-all",
        "--read-only",
        "--quiet",
    ];
    if args.first().map(String::as_str) != Some("run") {
        return Plan::Unknown;
    }
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if !a.starts_with('-') {
            return Plan::Image(a.into());
        }
        i += if a.contains('=') || BARE.contains(&a) {
            1
        } else {
            2
        };
    }
    Plan::Unknown
}

/// Moves every package-run server to its newest release, pulls images,
/// and writes the new versions into the agents. Returns how many failed.
pub fn update_mcp(store: &mut Store, remote: &dyn Remote, out: &mut dyn Write) -> Result<usize> {
    writeln!(out, "MCP servers")?;
    let servers: Vec<Server> = store.lib.mcp.iter().map(|e| e.server.clone()).collect();
    if servers.is_empty() {
        writeln!(out, "  none in the library")?;
        return Ok(0);
    }
    let width = servers.iter().map(|s| s.name.len()).max().unwrap_or(0);
    let (mut failed, mut changed) = (0, false);
    for s in servers {
        let line = match plan(&s) {
            Plan::Remote => "remote, nothing to update".to_string(),
            Plan::Unknown => format!("runs {}, not a package siu can update", s.command),
            Plan::Floating(spec) => format!("{spec} follows its releases already"),
            Plan::Image(image) => match remote.pull(&image) {
                Ok(()) => format!("pulled {image}"),
                Err(e) => {
                    failed += 1;
                    format!("couldn't pull {image}: {e:#}")
                }
            },
            Plan::Package {
                eco,
                name,
                version,
                at,
                prefix,
                base,
                sep,
            } => match remote.latest(eco, &name) {
                Err(e) => {
                    failed += 1;
                    format!("couldn't look up {name}: {e:#}")
                }
                Ok(v) if version.as_deref() == Some(v.as_str()) => {
                    format!("{name} {v} is the latest")
                }
                Ok(v) => {
                    let mut new = s.clone();
                    new.args[at] = format!("{prefix}{base}{sep}{v}");
                    store.edit_server(&s.name, new)?;
                    changed = true;
                    let from = version.unwrap_or_else(|| "unpinned".into());
                    format!("{name} {from} → {v}")
                }
            },
        };
        writeln!(out, "  {:<width$}  {line}", s.name)?;
    }
    if changed {
        let report = store.commit()?;
        for issue in &report.issues {
            writeln!(out, "  ! {issue}")?;
        }
        failed += report.issues.len();
    }
    Ok(failed)
}

// ---- skills -------------------------------------------------------------

/// Refreshes every skill from its source, one download per repository.
/// Returns how many failed.
pub fn update_skills(store: &mut Store, remote: &dyn Remote, out: &mut dyn Write) -> Result<usize> {
    writeln!(out, "Skills")?;
    let skills: Vec<(String, Option<Source>)> = store
        .lib
        .skills
        .iter()
        .map(|s| (s.name.clone(), s.source.clone()))
        .collect();
    if skills.is_empty() {
        writeln!(out, "  none in the library")?;
        return Ok(0);
    }
    let outcome =
        |store: &mut Store, name: &str, tree: &Path| match store.update_skill_from(name, tree) {
            Ok(UpdateOutcome::Updated) => Ok("updated".to_string()),
            Ok(UpdateOutcome::UpToDate) => Ok("up to date".to_string()),
            Err(e) => Err(format!("{e:#}")),
        };
    let mut lines: BTreeMap<String, Result<String, String>> = BTreeMap::new();
    let mut by_repo: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for (name, source) in skills {
        let line = match source {
            Some(Source::Github {
                repo, reference, ..
            }) => {
                by_repo.entry((repo, reference)).or_default().push(name);
                continue;
            }
            Some(Source::Local { dir }) => outcome(store, &name, &dir),
            None => Ok("imported, no source to update from".into()),
        };
        lines.insert(name, line);
    }
    for ((repo, reference), names) in by_repo {
        match remote.download(&repo, &reference) {
            Ok((_guard, root)) => {
                for n in names {
                    let line = outcome(store, &n, &root);
                    lines.insert(n, line);
                }
            }
            Err(e) => lines.extend(names.into_iter().map(|n| (n, Err(format!("{e:#}"))))),
        }
    }
    store.save()?;
    let width = lines.keys().map(String::len).max().unwrap_or(0);
    let mut failed = 0;
    for (name, line) in lines {
        let line = line.unwrap_or_else(|e| {
            failed += 1;
            e
        });
        writeln!(out, "  {name:<width$}  {line}")?;
    }
    Ok(failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::Transport;
    use crate::skills;
    use std::cell::RefCell;
    use std::collections::{BTreeSet, HashMap};
    use std::fs;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.0.4", "0.0.5"));
        assert!(newer("0.0.9", "0.0.10"));
        assert!(newer("0.9.0", "1.0.0"));
        assert!(!newer("0.0.4", "0.0.4"));
        assert!(!newer("0.1.0", "0.0.9"));
        assert!(newer("1.0.0-beta.1", "1.0.1"));
    }

    #[test]
    fn crates_io_answer_is_read() {
        let b: Body = serde_json::from_str(
            r#"{"crate":{"name":"siu-tui","max_stable_version":"0.0.4","max_version":"0.1.0-rc.1"}}"#,
        )
        .unwrap();
        assert_eq!(b.krate.max_stable_version.as_deref(), Some("0.0.4"));
    }

    fn stdio(command: &str, args: &[&str]) -> Server {
        Server {
            name: "s".into(),
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        }
    }

    /// The package a server runs and the arg its new version goes in.
    fn pkg(command: &str, args: &[&str]) -> Option<(String, Option<String>, String)> {
        let s = stdio(command, args);
        match plan(&s) {
            Plan::Package {
                name,
                version,
                at,
                prefix,
                base,
                sep,
                ..
            } => Some((name, version, format!("{at}:{prefix}{base}{sep}NEW"))),
            _ => None,
        }
    }

    #[test]
    fn plans_read_what_a_server_runs() {
        let some =
            |n: &str, v: Option<&str>, w: &str| Some((n.into(), v.map(Into::into), w.into()));
        assert_eq!(pkg("npx", &["-y", "foo"]), some("foo", None, "1:foo@NEW"));
        assert_eq!(
            pkg("npx", &["-y", "@a/b@1.2.3", "--x"]),
            some("@a/b", Some("1.2.3"), "1:@a/b@NEW")
        );
        assert_eq!(
            pkg("/usr/bin/npx", &["--package=@a/b@1.0.0", "b"]),
            some("@a/b", Some("1.0.0"), "0:--package=@a/b@NEW")
        );
        assert_eq!(
            pkg("npx", &["-y", "-p", "foo@2.0.0", "foo-cli"]),
            some("foo", Some("2.0.0"), "2:foo@NEW")
        );
        assert_eq!(
            pkg("uvx", &["mcp-time"]),
            some("mcp-time", None, "0:mcp-time@NEW")
        );
        assert_eq!(
            pkg("uvx", &["--python", "3.12", "mcp-time==1.0", "--tz", "x"]),
            some("mcp-time", Some("1.0"), "2:mcp-time==NEW")
        );
        assert_eq!(
            pkg("uvx", &["--from", "a[cli]==2.0", "a-cli"]),
            some("a", Some("2.0"), "1:a[cli]==NEW")
        );
        for floating in [
            stdio("npx", &["-y", "foo@latest"]),
            stdio("npx", &["foo@^1.2.0"]),
            stdio("npx", &["foo@1"]),
            stdio("uvx", &["a>=1"]),
            stdio("uvx", &["a@latest"]),
        ] {
            assert!(matches!(plan(&floating), Plan::Floating(_)), "{floating:?}");
        }
        for unknown in [
            stdio("node", &["server.js"]),
            stdio("npx", &["./local"]),
            stdio("npx", &["-c", "foo"]),
            stdio("npx", &["git+https://x/y"]),
            stdio("uvx", &["--from", "git+https://x/y", "y"]),
            stdio("docker", &["ps"]),
        ] {
            assert_eq!(plan(&unknown), Plan::Unknown, "{unknown:?}");
        }
        assert_eq!(
            plan(&stdio(
                "docker",
                &[
                    "run",
                    "-i",
                    "--rm",
                    "-e",
                    "TOKEN",
                    "--name=x",
                    "ghcr.io/a/b:1",
                    "stdio"
                ]
            )),
            Plan::Image("ghcr.io/a/b:1".into())
        );
    }

    #[derive(Default)]
    struct Fake {
        versions: HashMap<&'static str, &'static str>,
        trees: HashMap<&'static str, PathBuf>,
        pulled: RefCell<Vec<String>>,
        downloads: RefCell<usize>,
    }

    impl Remote for Fake {
        fn latest(&self, _: Eco, name: &str) -> Result<String> {
            match self.versions.get(name) {
                Some(v) => Ok(v.to_string()),
                None => bail!("{name} not found"),
            }
        }
        fn pull(&self, image: &str) -> Result<()> {
            self.pulled.borrow_mut().push(image.into());
            Ok(())
        }
        fn download(&self, repo: &str, _: &str) -> Result<(tempfile::TempDir, PathBuf)> {
            *self.downloads.borrow_mut() += 1;
            match self.trees.get(repo) {
                Some(t) => Ok((tempfile::tempdir()?, t.clone())),
                None => bail!("couldn't download {repo}"),
            }
        }
    }

    /// A home with Claude Code and Codex installed.
    fn home() -> (tempfile::TempDir, PathBuf, Store) {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().to_path_buf();
        for dir in [".claude", ".codex"] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        let store = Store::open(&Env::rooted(&home), home.join(".siu")).unwrap();
        (d, home, store)
    }

    fn claude_args(home: &Path, name: &str) -> Vec<String> {
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        serde_json::from_value(v["mcpServers"][name]["args"].clone()).unwrap()
    }

    #[test]
    fn mcp_servers_move_to_their_newest_releases() {
        let (_d, home, mut store) = home();
        let servers = [
            ("pinned", stdio("npx", &["-y", "@a/pinned@1.0.0"])),
            ("loose", stdio("npx", &["-y", "loose"])),
            ("tagged", stdio("npx", &["-y", "tagged@latest"])),
            ("current", stdio("uvx", &["current==2.0"])),
            ("gone", stdio("npx", &["gone"])),
            (
                "img",
                stdio("docker", &["run", "-i", "--rm", "ghcr.io/a/img"]),
            ),
            ("own", stdio("node", &["/srv/own.js"])),
            (
                "web",
                Server {
                    transport: Transport::Http,
                    url: "https://example.com/mcp".into(),
                    ..Default::default()
                },
            ),
        ];
        for (name, mut s) in servers {
            s.name = name.into();
            let agents = store.default_server_agents(&s);
            store.add_server(s, agents, None).unwrap();
        }
        store.commit().unwrap();
        let fake = Fake {
            versions: HashMap::from([
                ("@a/pinned", "1.4.0"),
                ("loose", "3.1.0"),
                ("current", "2.0"),
            ]),
            ..Default::default()
        };
        let mut out = vec![];
        let failed = update_mcp(&mut store, &fake, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();

        assert_eq!(failed, 1, "{out}");
        for line in [
            "pinned   @a/pinned 1.0.0 → 1.4.0",
            "loose    loose unpinned → 3.1.0",
            "tagged   tagged@latest follows its releases already",
            "current  current 2.0 is the latest",
            "gone     couldn't look up gone: gone not found",
            "img      pulled ghcr.io/a/img",
            "own      runs node, not a package siu can update",
            "web      remote, nothing to update",
        ] {
            assert!(out.contains(line), "{line:?} not in\n{out}");
        }
        assert_eq!(claude_args(&home, "pinned"), ["-y", "@a/pinned@1.4.0"]);
        assert_eq!(claude_args(&home, "loose"), ["-y", "loose@3.1.0"]);
        assert_eq!(claude_args(&home, "tagged"), ["-y", "tagged@latest"]);
        let codex = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(codex.contains("@a/pinned@1.4.0"), "{codex}");
        // kept in the library too, so the next launch shows it
        let again = Store::open(&Env::rooted(&home), home.join(".siu")).unwrap();
        assert_eq!(
            again.server("loose").unwrap().server.args,
            ["-y", "loose@3.1.0"]
        );
    }

    fn write_skill(dir: &Path, body: &str) {
        fs::create_dir_all(dir).unwrap();
        let name = dir.file_name().unwrap().to_str().unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: about {name}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn skills_update_from_their_sources() {
        let (_d, home, mut store) = home();
        let repo = home.join("repo");
        write_skill(&repo.join("skills/pdf"), "v1");
        write_skill(&repo.join("skills/docx"), "v1");
        let local = home.join("mine/notes");
        write_skill(&local, "v1");
        write_skill(&home.join("gone/old"), "v1");
        let agents: BTreeSet<String> = store.default_skill_agents();
        let gh = |repo: &'static str| {
            move |f: &skills::Found| {
                Some(Source::Github {
                    repo: repo.into(),
                    reference: "HEAD".into(),
                    path: f.path.clone(),
                })
            }
        };
        store
            .install_skills(&skills::find(&repo), gh("a/repo"), &agents)
            .unwrap();
        store
            .install_skills(&skills::find(&home.join("gone")), gh("a/gone"), &agents)
            .unwrap();
        store
            .install_skills(
                &skills::find(&local),
                |_| Some(Source::Local { dir: local.clone() }),
                &agents,
            )
            .unwrap();
        store.commit().unwrap();

        write_skill(&repo.join("skills/pdf"), "v2");
        write_skill(&local, "v2");
        let fake = Fake {
            trees: HashMap::from([("a/repo", repo.clone())]),
            ..Default::default()
        };
        let mut out = vec![];
        let failed = update_skills(&mut store, &fake, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();

        assert_eq!(failed, 1, "{out}");
        assert_eq!(*fake.downloads.borrow(), 2, "one download per repository");
        for line in [
            "docx   up to date",
            "notes  updated",
            "old    couldn't download a/gone",
            "pdf    updated",
        ] {
            assert!(out.contains(line), "{line:?} not in\n{out}");
        }
        for name in ["pdf", "notes"] {
            let md = fs::read_to_string(home.join(".claude/skills").join(name).join("SKILL.md"))
                .unwrap();
            assert!(md.ends_with("v2"), "{name}: {md}");
        }
    }

    /// `cargo test -- --ignored online`.
    #[test]
    #[ignore]
    fn online_latest_versions() {
        let v = latest().unwrap();
        assert!(
            !newer(&v, CURRENT),
            "crates.io has {v}, older than {CURRENT}"
        );
        for (eco, name) in [
            (Eco::Npm, "@upstash/context7-mcp"),
            (Eco::Pypi, "mcp-server-time"),
        ] {
            let v = Net.latest(eco, name).unwrap();
            assert!(v.starts_with(|c: char| c.is_ascii_digit()), "{name}: {v}");
        }
    }
}
