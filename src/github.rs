//! Fetching skills from GitHub: a repository's files in one tarball.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;

use crate::net;

/// Where skills are to be found: a repository, optionally a ref and a
/// folder in it, or a folder on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    Github {
        repo: String,
        reference: String,
        subpath: String,
    },
    Local(PathBuf),
}

/// Reads `owner/repo`, `owner/repo/path`, a github.com URL (with
/// `/tree/<ref>/<path>`) or a local folder.
pub fn parse_spec(input: &str, home: &std::path::Path) -> Option<Spec> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    if s.starts_with('/') || s.starts_with("~/") || s.starts_with("./") || s == "~" {
        let p = match s.strip_prefix("~") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(s),
        };
        return Some(Spec::Local(p));
    }
    let s = s.trim_end_matches(".git");
    let rest = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))
        .unwrap_or(s);
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2
        || !parts[..2].iter().all(|p| {
            p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    {
        return None;
    }
    let repo = format!("{}/{}", parts[0], parts[1]);
    let (reference, subpath) = match parts.get(2) {
        Some(&"tree") | Some(&"blob") if parts.len() >= 4 => {
            (parts[3].to_string(), parts[4..].join("/"))
        }
        _ => ("HEAD".to_string(), parts[2..].join("/")),
    };
    let subpath = subpath
        .trim_end_matches("/SKILL.md")
        .trim_end_matches("SKILL.md")
        .trim_end_matches('/')
        .to_string();
    Some(Spec::Github {
        repo,
        reference,
        subpath,
    })
}

/// A repository's files, unpacked into a temporary folder that lives as
/// long as the returned guard. The path is the repository's root.
pub fn download(repo: &str, reference: &str) -> Result<(tempfile::TempDir, PathBuf)> {
    let url = format!(
        "https://codeload.github.com/{repo}/tar.gz/{}",
        net::encode(reference)
    );
    let bytes = net::get(&url, "application/gzip", 256 << 20)
        .with_context(|| format!("couldn't download {repo}"))?;
    let tmp = tempfile::Builder::new().prefix("siu-").tempdir()?;
    tar::Archive::new(GzDecoder::new(&bytes[..]))
        .unpack(tmp.path())
        .with_context(|| format!("couldn't unpack {repo}"))?;
    // the tarball holds one folder, <repo>-<sha>
    let mut dirs = std::fs::read_dir(tmp.path())?
        .flatten()
        .filter(|e| e.path().is_dir());
    let Some(root) = dirs.next().map(|e| e.path()) else {
        bail!("{repo}'s tarball was empty")
    };
    Ok((tmp, root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn gh(repo: &str, r: &str, sub: &str) -> Option<Spec> {
        Some(Spec::Github {
            repo: repo.into(),
            reference: r.into(),
            subpath: sub.into(),
        })
    }

    #[test]
    fn specs() {
        let home = Path::new("/home/u");
        assert_eq!(
            parse_spec("anthropics/skills", home),
            gh("anthropics/skills", "HEAD", "")
        );
        assert_eq!(
            parse_spec("anthropics/skills/skills/pdf", home),
            gh("anthropics/skills", "HEAD", "skills/pdf")
        );
        assert_eq!(
            parse_spec("https://github.com/a/b.git", home),
            gh("a/b", "HEAD", "")
        );
        assert_eq!(
            parse_spec("https://github.com/a/b/tree/main/skills/pdf/", home),
            gh("a/b", "main", "skills/pdf")
        );
        assert_eq!(
            parse_spec("https://github.com/a/b/blob/v1/x/SKILL.md", home),
            gh("a/b", "v1", "x")
        );
        assert_eq!(
            parse_spec("~/code/skills", home),
            Some(Spec::Local("/home/u/code/skills".into()))
        );
        assert_eq!(
            parse_spec("/tmp/s", home),
            Some(Spec::Local("/tmp/s".into()))
        );
        assert_eq!(parse_spec("nope", home), None);
        assert_eq!(parse_spec("a b/c", home), None);
    }
}
