//! A proxy for everything mapterm fetches: map tiles, imagery, the live feeds and the
//! WebSocket streams. Set it with [`set`] (the viewer's `--proxy URL`) or `MAPTERM_PROXY`,
//! before the first request:
//!
//! - `socks5h://HOST:PORT`: SOCKS5, the proxy looks up host names (nothing about where
//!   you're going leaves this machine except to the proxy). The one to use, usually.
//! - `socks5://HOST:PORT`: SOCKS5, host names looked up here.
//! - `http://HOST:PORT`: an HTTP proxy (`CONNECT` for TLS and WebSockets).
//!
//! A user name and password go in the URL (`socks5h://user:pass@host:1080`). Local
//! addresses (localhost, 127.0.0.1, ::1) never go through it.

use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A proxy: its URL, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proxy {
    url: reqwest::Url,
}

/// The kinds of proxy mapterm speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// SOCKS5, host names looked up by the proxy.
    Socks5h,
    /// SOCKS5, host names looked up here.
    Socks5,
    /// An HTTP proxy.
    Http,
}

impl Proxy {
    /// A proxy from its URL: `socks5h://`, `socks5://` or `http://` with a host and port.
    pub fn parse(url: &str) -> Result<Proxy> {
        let u = reqwest::Url::parse(url.trim()).with_context(|| format!("proxy URL {:?}", redact(url)))?;
        if !matches!(u.scheme(), "socks5h" | "socks5" | "http") {
            bail!("a proxy is socks5h://, socks5:// or http://, not {}://", u.scheme());
        }
        if u.host_str().is_none_or(str::is_empty) {
            bail!("the proxy URL needs a host");
        }
        Ok(Proxy { url: u })
    }

    fn kind(&self) -> Kind {
        match self.url.scheme() {
            "socks5h" => Kind::Socks5h,
            "socks5" => Kind::Socks5,
            _ => Kind::Http,
        }
    }

    fn host(&self) -> String {
        self.url.host_str().unwrap_or_default().trim_matches(['[', ']']).to_string()
    }

    fn port(&self) -> u16 {
        // As reqwest reads the same URL: HTTP's 80, SOCKS's 1080.
        self.url.port_or_known_default().unwrap_or(1080)
    }

    /// The user name and password in the URL, if any.
    fn login(&self) -> Option<(String, String)> {
        (!self.url.username().is_empty()).then(|| (unescape(self.url.username()), unescape(self.url.password().unwrap_or(""))))
    }

    /// The URL without its password, for logs and messages.
    pub fn display(&self) -> String {
        redact(self.url.as_str())
    }

    /// The same proxy for reqwest, local addresses excepted.
    fn for_reqwest(&self) -> Result<reqwest::Proxy> {
        Ok(reqwest::Proxy::all(self.url.as_str())?.no_proxy(reqwest::NoProxy::from_string("localhost,127.0.0.1,::1")))
    }

    /// A TCP connection to `host:port` through the proxy.
    pub async fn connect(&self, host: &str, port: u16) -> Result<TcpStream> {
        let mut s = TcpStream::connect((self.host(), self.port())).await.with_context(|| format!("connecting to the proxy {}", self.display()))?;
        match self.kind() {
            Kind::Socks5h => socks5(&mut s, host, port, self.login(), true).await,
            Kind::Socks5 => socks5(&mut s, host, port, self.login(), false).await,
            Kind::Http => http_connect(&mut s, host, port, self.login()).await,
        }
        .with_context(|| format!("through the proxy {}", self.display()))?;
        Ok(s)
    }
}

static PROXY: OnceLock<Option<Proxy>> = OnceLock::new();

/// Send everything through `url` from now on. Call it before the first request: the HTTP
/// clients are made once and keep what they were made with.
pub fn set(url: &str) -> Result<()> {
    let p = Proxy::parse(url)?;
    if PROXY.set(Some(p.clone())).is_err() && get() != Some(&p) {
        bail!("set the proxy before the first request");
    }
    Ok(())
}

/// The proxy in use: the one [`set`] gave, else `MAPTERM_PROXY`'s, else none. A
/// `MAPTERM_PROXY` that doesn't parse is an error the first time, not silently ignored:
/// going direct when someone asked for a proxy is the wrong way to fail.
pub fn get() -> Option<&'static Proxy> {
    PROXY
        .get_or_init(|| {
            let v = std::env::var("MAPTERM_PROXY").ok().filter(|v| !v.trim().is_empty())?;
            Some(Proxy::parse(&v).unwrap_or_else(|e| panic!("MAPTERM_PROXY: {e:#}")))
        })
        .as_ref()
}

/// `b` with the proxy in use, if any.
pub(crate) fn apply(b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    match get() {
        Some(p) => apply_to(b, p),
        None => b,
    }
}

fn apply_to(b: reqwest::ClientBuilder, p: &Proxy) -> reqwest::ClientBuilder {
    match p.for_reqwest() {
        Ok(rp) => b.proxy(rp),
        // parse() checked it; reqwest refusing it anyway must not mean going direct.
        Err(e) => panic!("proxy {}: {e}", p.display()),
    }
}

/// A TCP connection to `host:port`: through the proxy in use, unless the host is local.
pub(crate) async fn tcp(host: &str, port: u16) -> Result<TcpStream> {
    match get() {
        Some(p) if !is_local(host) => p.connect(host, port).await,
        _ => TcpStream::connect((host, port)).await.with_context(|| format!("connecting to {host}:{port}")),
    }
}

fn is_local(host: &str) -> bool {
    matches!(host.trim_matches(['[', ']']), "localhost" | "127.0.0.1" | "::1")
}

/// SOCKS5 (RFC 1928), with a user name and password (RFC 1929) when given.
async fn socks5(s: &mut TcpStream, host: &str, port: u16, login: Option<(String, String)>, remote_dns: bool) -> Result<()> {
    s.write_all(if login.is_some() { &[5, 2, 0, 2] } else { &[5, 1, 0] }).await?;
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await.context("SOCKS greeting")?;
    if r[0] != 5 {
        bail!("not a SOCKS5 proxy");
    }
    match (r[1], &login) {
        (0, _) => {}
        (2, Some((user, pass))) => {
            if user.len() > 255 || pass.len() > 255 {
                bail!("SOCKS user name and password are at most 255 bytes");
            }
            let mut m = vec![1, user.len() as u8];
            m.extend_from_slice(user.as_bytes());
            m.push(pass.len() as u8);
            m.extend_from_slice(pass.as_bytes());
            s.write_all(&m).await?;
            s.read_exact(&mut r).await.context("SOCKS login")?;
            if r[1] != 0 {
                bail!("the proxy refused the user name and password");
            }
        }
        (2, None) => bail!("the proxy wants a user name and password (socks5h://user:pass@host:port)"),
        _ => bail!("the proxy accepts none of our ways to sign in"),
    }
    let host = host.trim_matches(['[', ']']);
    let mut m = vec![5, 1, 0];
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(a)) => {
            m.push(1);
            m.extend_from_slice(&a.octets());
        }
        Ok(std::net::IpAddr::V6(a)) => {
            m.push(4);
            m.extend_from_slice(&a.octets());
        }
        Err(_) if remote_dns => {
            if host.len() > 255 {
                bail!("host name too long for SOCKS");
            }
            m.push(3);
            m.push(host.len() as u8);
            m.extend_from_slice(host.as_bytes());
        }
        Err(_) => {
            let addr = tokio::net::lookup_host((host, port)).await?.next().with_context(|| format!("no address for {host}"))?;
            match addr.ip() {
                std::net::IpAddr::V4(a) => {
                    m.push(1);
                    m.extend_from_slice(&a.octets());
                }
                std::net::IpAddr::V6(a) => {
                    m.push(4);
                    m.extend_from_slice(&a.octets());
                }
            }
        }
    }
    m.extend_from_slice(&port.to_be_bytes());
    s.write_all(&m).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await.context("SOCKS reply")?;
    if head[1] != 0 {
        let why = match head[1] {
            1 => "general failure",
            2 => "not allowed by its rules",
            3 => "network unreachable",
            4 => "host unreachable",
            5 => "connection refused",
            6 => "timed out",
            7 => "command not supported",
            8 => "address type not supported",
            _ => "unknown error",
        };
        bail!("the proxy couldn't reach {host}:{port}: {why}");
    }
    // The address it bound, which we don't need.
    let len = match head[3] {
        1 => 4,
        4 => 16,
        3 => s.read_u8().await? as usize,
        t => bail!("SOCKS reply with address type {t}"),
    };
    let mut skip = vec![0u8; len + 2];
    s.read_exact(&mut skip).await?;
    Ok(())
}

/// An HTTP proxy's `CONNECT`.
async fn http_connect(s: &mut TcpStream, host: &str, port: u16, login: Option<(String, String)>) -> Result<()> {
    let authority = if host.contains(':') && !host.starts_with('[') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some((user, pass)) = login {
        req += &format!("Proxy-Authorization: Basic {}\r\n", crate::kitty::base64(format!("{user}:{pass}").as_bytes()));
    }
    req += "\r\n";
    s.write_all(req.as_bytes()).await?;
    // Read the reply's head a byte at a time, so nothing after it is taken from the stream.
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            bail!("the proxy's reply is too long");
        }
        head.push(s.read_u8().await.context("the proxy's reply")?);
    }
    let status = String::from_utf8_lossy(&head);
    let line = status.lines().next().unwrap_or_default();
    if line.split_whitespace().nth(1) != Some("200") {
        bail!("the proxy said {line:?}");
    }
    Ok(())
}

/// `%40` as `@`, and so on, for user names and passwords in a URL.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A URL with any password replaced by `***`.
fn redact(url: &str) -> String {
    match reqwest::Url::parse(url.trim()) {
        Ok(mut u) if u.password().is_some() => {
            let _ = u.set_password(Some("***"));
            u.to_string()
        }
        Ok(u) => u.to_string(),
        Err(_) => "(not a URL)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn parses_and_redacts() {
        let p = Proxy::parse("socks5h://127.0.0.1:1080").unwrap();
        assert_eq!((p.kind(), p.host(), p.port()), (Kind::Socks5h, "127.0.0.1".to_string(), 1080));
        let p = Proxy::parse("socks5h://me:p%40ss@proxy.example:9050").unwrap();
        assert_eq!(p.login(), Some(("me".to_string(), "p@ss".to_string())));
        assert!(!p.display().contains("p%40ss") && p.display().contains("***"), "{}", p.display());
        assert_eq!(Proxy::parse("http://proxy.example").unwrap().port(), 80);
        assert_eq!(Proxy::parse("socks5h://proxy.example").unwrap().port(), 1080);
        assert!(Proxy::parse("ftp://x:1").is_err());
        assert!(Proxy::parse("socks5h://").is_err());
        // An error never shows the password.
        let e = format!("{:#}", Proxy::parse("socks5h://me:secret@[bad").unwrap_err());
        assert!(!e.contains("secret"), "{e}");
    }

    /// An echo server, and a SOCKS5 proxy in front of it that checks what it's asked for.
    async fn socks_proxy(want_login: Option<(&'static str, &'static str)>) -> (u16, tokio::task::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let (mut c, _) = l.accept().await.unwrap();
            let mut g = [0u8; 2];
            c.read_exact(&mut g).await.unwrap();
            let mut methods = vec![0u8; g[1] as usize];
            c.read_exact(&mut methods).await.unwrap();
            if let Some((user, pass)) = want_login {
                assert!(methods.contains(&2));
                c.write_all(&[5, 2]).await.unwrap();
                let mut v = [0u8; 2];
                c.read_exact(&mut v).await.unwrap();
                let mut u = vec![0u8; v[1] as usize];
                c.read_exact(&mut u).await.unwrap();
                let pl = c.read_u8().await.unwrap();
                let mut pw = vec![0u8; pl as usize];
                c.read_exact(&mut pw).await.unwrap();
                assert_eq!((u.as_slice(), pw.as_slice()), (user.as_bytes(), pass.as_bytes()));
                c.write_all(&[1, 0]).await.unwrap();
            } else {
                c.write_all(&[5, 0]).await.unwrap();
            }
            let mut req = [0u8; 4];
            c.read_exact(&mut req).await.unwrap();
            let mut asked = req.to_vec();
            let n = match req[3] {
                1 => 4,
                4 => 16,
                _ => {
                    let n = c.read_u8().await.unwrap();
                    asked.push(n);
                    n as usize
                }
            };
            let mut rest = vec![0u8; n + 2];
            c.read_exact(&mut rest).await.unwrap();
            asked.extend_from_slice(&rest);
            c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
            // Then echo.
            let mut buf = [0u8; 5];
            c.read_exact(&mut buf).await.unwrap();
            c.write_all(&buf).await.unwrap();
            asked
        });
        (port, h)
    }

    #[tokio::test]
    async fn socks5h_sends_the_host_name() {
        let (port, h) = socks_proxy(None).await;
        let p = Proxy::parse(&format!("socks5h://127.0.0.1:{port}")).unwrap();
        let mut s = p.connect("db.example.org", 443).await.unwrap();
        s.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        s.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");
        let asked = h.await.unwrap();
        let mut want = vec![5, 1, 0, 3, 14];
        want.extend_from_slice(b"db.example.org");
        want.extend_from_slice(&443u16.to_be_bytes());
        assert_eq!(asked, want, "the name, not an address: the proxy looks it up");
    }

    #[tokio::test]
    async fn socks5_with_a_login() {
        let (port, h) = socks_proxy(Some(("me", "p@ss"))).await;
        let p = Proxy::parse(&format!("socks5h://me:p%40ss@127.0.0.1:{port}")).unwrap();
        let mut s = p.connect("10.1.2.3", 80).await.unwrap();
        s.write_all(b"12345").await.unwrap();
        let asked = h.await.unwrap();
        assert_eq!(asked, vec![5, 1, 0, 1, 10, 1, 2, 3, 0, 80], "an address stays an address");
    }

    #[tokio::test]
    async fn http_connect_and_its_refusal() {
        for (reply, ok) in [("HTTP/1.1 200 Connection established\r\n\r\n", true), ("HTTP/1.1 403 Forbidden\r\n\r\n", false)] {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            let h = tokio::spawn(async move {
                let (mut c, _) = l.accept().await.unwrap();
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    head.push(c.read_u8().await.unwrap());
                }
                c.write_all(reply.as_bytes()).await.unwrap();
                String::from_utf8(head).unwrap()
            });
            let p = Proxy::parse(&format!("http://u:pw@127.0.0.1:{port}")).unwrap();
            let r = p.connect("db.example.org", 443).await;
            assert_eq!(r.is_ok(), ok, "{r:?}");
            let head = h.await.unwrap();
            assert!(head.starts_with("CONNECT db.example.org:443 HTTP/1.1\r\n"), "{head}");
            assert!(head.contains(&format!("Proxy-Authorization: Basic {}", crate::kitty::base64(b"u:pw"))));
            if let Err(e) = r {
                assert!(format!("{e:#}").contains("403") && !format!("{e:#}").contains("pw"), "{e:#}");
            }
        }
    }

    #[tokio::test]
    async fn reqwest_goes_through_it() {
        // An HTTP request through a SOCKS5 proxy: the proxy sees the host name and the
        // request reaches it.
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let (mut c, _) = l.accept().await.unwrap();
            let mut g = [0u8; 2];
            c.read_exact(&mut g).await.unwrap();
            let mut m = vec![0u8; g[1] as usize];
            c.read_exact(&mut m).await.unwrap();
            c.write_all(&[5, 0]).await.unwrap();
            let mut req = [0u8; 5];
            c.read_exact(&mut req).await.unwrap();
            let mut name = vec![0u8; req[4] as usize + 2];
            c.read_exact(&mut name).await.unwrap();
            c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(c.read_u8().await.unwrap());
            }
            c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok").await.unwrap();
            (String::from_utf8_lossy(&name[..name.len() - 2]).into_owned(), String::from_utf8(head).unwrap())
        });
        let p = Proxy::parse(&format!("socks5h://127.0.0.1:{port}")).unwrap();
        let client = apply_to(reqwest::Client::builder(), &p).build().unwrap();
        let body = client.get("http://tiles.example.org/0/0/0.pbf").send().await.unwrap().text().await.unwrap();
        assert_eq!(body, "ok");
        let (name, head) = h.await.unwrap();
        assert_eq!(name, "tiles.example.org");
        assert!(head.starts_with("GET /0/0/0.pbf HTTP/1.1"), "{head}");
    }

    #[test]
    fn local_hosts_go_direct() {
        assert!(is_local("127.0.0.1") && is_local("localhost") && is_local("[::1]"));
        assert!(!is_local("db.satnogs.org"));
    }
}
