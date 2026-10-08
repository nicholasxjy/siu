//! HTTP: one agent for every request, with a timeout.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};

// skills.sh answers a client it doesn't know with a page of its own
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36 siu";

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(UA)
            .build()
            .into()
    })
}

pub fn get(url: &str, accept: &str, limit: u64) -> Result<Vec<u8>> {
    let resp = agent()
        .get(url)
        .header("Accept", accept)
        .call()
        .with_context(|| format!("GET {}", host(url)))?;
    let mut out = vec![];
    resp.into_body()
        .into_with_config()
        .limit(limit + 1)
        .reader()
        .read_to_end(&mut out)
        .with_context(|| format!("reading {}", host(url)))?;
    if out.len() as u64 > limit {
        bail!("{} sent more than {} MB", host(url), limit >> 20);
    }
    Ok(out)
}

pub fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let b = get(url, "application/json", 16 << 20)?;
    serde_json::from_slice(&b)
        .with_context(|| format!("{} answered something unexpected", host(url)))
}

fn host(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or(url)
}

/// Percent-encodes a query value.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A SOCKS5 proxy (ALL_PROXY=socks5://…, as Clash and others set it)
    /// is used, not bypassed: going direct is what timed out downloads.
    #[test]
    fn requests_go_through_a_socks5_proxy() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 512];
            // greeting: no authentication
            let n = s.read(&mut b).unwrap();
            assert_eq!(b[0], 5, "{:?}", &b[..n]);
            s.write_all(&[5, 0]).unwrap();
            // CONNECT, then answer the request ourselves
            let n = s.read(&mut b).unwrap();
            assert_eq!(&b[..2], &[5, 1], "{:?}", &b[..n]);
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
            let n = s.read(&mut b).unwrap();
            assert!(b[..n].starts_with(b"GET /x "));
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(Some(
                ureq::Proxy::new(&format!("socks5://127.0.0.1:{port}")).unwrap(),
            ))
            .build()
            .into();
        let body = agent
            .get("http://localhost/x")
            .call()
            .unwrap()
            .into_body()
            .read_to_string()
            .unwrap();
        assert_eq!(body, "ok");
        server.join().unwrap();
    }
}
