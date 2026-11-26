//! A small WebSocket client (RFC 6455) with `permessage-deflate` (RFC 7692), for the live
//! feeds: aisstream.io and SondeHub both ask clients to take their streams compressed, and
//! tungstenite has no extension support (it rejects compressed frames outright).
//!
//! What a feed client needs and no more: `wss://` (rustls, trusting the platform's
//! certificates) or `ws://`, a subprotocol, text and binary messages in and out (fragmented
//! or not), ping and close handled, and compressed messages from the server inflated with a
//! window kept across messages (or reset per message when the server says
//! `server_no_context_takeover`). Messages it sends go uncompressed, which RFC 7692 allows:
//! a feed client only sends small subscriptions.

use std::io;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest message read, compressed or not (a feed's message is a few KB).
const MAX_MESSAGE: usize = 16 << 20;
/// Largest handshake response read.
const MAX_HEADERS: usize = 16 << 10;

/// A message from the server.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A text message.
    Text(String),
    /// A binary message.
    Binary(Vec<u8>),
    /// The server closed the connection, with its code and reason if it gave them.
    Close(Option<(u16, String)>),
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// An open WebSocket.
pub struct WebSocket {
    io: Box<dyn Io>,
    /// Bytes read and not yet parsed.
    buf: Vec<u8>,
    /// The negotiated deflate, if any.
    inflate: Option<Inflate>,
    /// A fragmented message being put together: opcode, compressed, bytes.
    partial: Option<(u8, bool, Vec<u8>)>,
    closed: bool,
}

/// One frame off the wire.
struct Frame {
    fin: bool,
    /// Compressed (permessage-deflate), on a message's first frame.
    rsv1: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// permessage-deflate as the server set it up.
struct Inflate {
    z: flate2::Decompress,
    /// Start every message with a fresh window.
    reset: bool,
}

/// What the handshake asked the server for.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    /// `Sec-WebSocket-Protocol` (such as `mqtt`).
    pub protocol: Option<&'a str>,
    /// Ask for permessage-deflate.
    pub deflate: bool,
}

/// Sixteen bytes that aren't predictable (the handshake key and frame masks): hashed by
/// std's randomly keyed hasher with a counter and the time.
fn random16() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut out = [0u8; 16];
    for half in out.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// The `Sec-WebSocket-Accept` a server must answer `key` with.
fn accept_for(key: &str) -> String {
    use sha1::Digest;
    let mut h = sha1::Sha1::new();
    h.update(key.as_bytes());
    h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    crate::kitty::base64(&h.finalize())
}

/// TLS settings: the platform's certificates, HTTP/1.1.
fn tls() -> Result<tokio_rustls::TlsConnector> {
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    let found = rustls_native_certs::load_native_certs();
    for cert in found.certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        bail!("no trusted certificates found on this system");
    }
    let mut cfg = tokio_rustls::rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(tokio_rustls::TlsConnector::from(Arc::new(cfg)))
}

/// Connect to `url` (`wss://` or `ws://`).
pub async fn connect(url: &str, opts: Options<'_>) -> Result<WebSocket> {
    let (tls_on, rest) = if let Some(r) = url.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("ws://") {
        (false, r)
    } else {
        bail!("not a WebSocket URL: {url}");
    };
    let (authority, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(a, p)| (a, format!("/{p}")));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h.to_string(), p.parse::<u16>().context("WebSocket port")?),
        _ => (authority.to_string(), if tls_on { 443 } else { 80 }),
    };
    // Through the proxy, if one is set.
    let tcp = crate::proxy::tcp(host.trim_matches(['[', ']']), port).await?;
    let _ = tcp.set_nodelay(true);
    let io: Box<dyn Io> = if tls_on {
        let name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.trim_matches(['[', ']']).to_string()).context("server name")?;
        Box::new(tls()?.connect(name, tcp).await.context("TLS")?)
    } else {
        Box::new(tcp)
    };
    handshake(io, &host, port, tls_on, &path, opts).await
}

async fn handshake(mut io: Box<dyn Io>, host: &str, port: u16, tls_on: bool, path: &str, opts: Options<'_>) -> Result<WebSocket> {
    let key = crate::kitty::base64(&random16());
    let host_header = if port == if tls_on { 443 } else { 80 } { host.to_string() } else { format!("{host}:{port}") };
    let mut req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: mapterm/{}\r\n",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(p) = opts.protocol {
        req += &format!("Sec-WebSocket-Protocol: {p}\r\n");
    }
    if opts.deflate {
        // Any window size the server likes; we don't compress what we send.
        req += "Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits\r\n";
    }
    req += "\r\n";
    io.write_all(req.as_bytes()).await?;
    io.flush().await?;
    // The response headers, and whatever frames came right behind them.
    let mut buf = Vec::new();
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > MAX_HEADERS {
            bail!("WebSocket handshake: response headers too long");
        }
        let mut chunk = [0u8; 4096];
        let n = io.read(&mut chunk).await?;
        if n == 0 {
            bail!("WebSocket handshake: the server closed the connection");
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    buf.drain(..end);
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or("");
    if status.split_whitespace().nth(1) != Some("101") {
        bail!("WebSocket handshake refused: {status}");
    }
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    let get = |k: &str| headers.iter().filter(|(h, _)| h == k).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
    if get("sec-websocket-accept").first().copied() != Some(accept_for(&key).as_str()) {
        bail!("WebSocket handshake: wrong Sec-WebSocket-Accept");
    }
    // The extension the server chose, if any; anything we didn't ask for is an error.
    let mut inflate = None;
    for ext in get("sec-websocket-extensions").iter().flat_map(|v| v.split(',')) {
        let mut params = ext.split(';').map(str::trim);
        match params.next() {
            Some("permessage-deflate") if opts.deflate && inflate.is_none() => {
                let mut reset = false;
                for p in params {
                    let name = p.split('=').next().unwrap_or("").trim();
                    match name {
                        "server_no_context_takeover" => reset = true,
                        // Our decompressor takes any window up to 15 bits; we don't compress.
                        "client_no_context_takeover" | "server_max_window_bits" | "client_max_window_bits" => {}
                        other => bail!("WebSocket handshake: unknown permessage-deflate parameter {other:?}"),
                    }
                }
                inflate = Some(Inflate { z: flate2::Decompress::new(false), reset });
            }
            Some("") | None => {}
            Some(other) => bail!("WebSocket handshake: the server chose an extension we didn't ask for: {other}"),
        }
    }
    Ok(WebSocket { io, buf, inflate, partial: None, closed: false })
}

impl WebSocket {
    /// Whether the server agreed to compress.
    pub fn compressed(&self) -> bool {
        self.inflate.is_some()
    }

    /// Send a text message.
    pub async fn send_text(&mut self, s: &str) -> Result<()> {
        self.send_frame(0x1, s.as_bytes()).await
    }

    /// Send a binary message.
    pub async fn send_binary(&mut self, b: &[u8]) -> Result<()> {
        self.send_frame(0x2, b).await
    }

    /// Close the connection (politely: a close frame, then the socket).
    pub async fn close(&mut self) -> Result<()> {
        if !self.closed {
            self.closed = true;
            let _ = self.send_frame(0x8, &1000u16.to_be_bytes()).await;
        }
        let _ = self.io.shutdown().await;
        Ok(())
    }

    /// One frame, final, masked as a client's must be.
    async fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        let mut f = Vec::with_capacity(payload.len() + 14);
        f.push(0x80 | opcode);
        match payload.len() {
            n if n < 126 => f.push(0x80 | n as u8),
            n if n <= 0xffff => {
                f.push(0x80 | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(0x80 | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        let mask: [u8; 4] = random16()[..4].try_into().unwrap_or([0x5a; 4]);
        f.extend_from_slice(&mask);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.io.write_all(&f).await?;
        self.io.flush().await?;
        Ok(())
    }

    /// The next message (pings answered, fragments joined, compressed ones inflated), or
    /// None once the connection has closed.
    pub async fn next(&mut self) -> Result<Option<Message>> {
        loop {
            if let Some(Frame { fin, rsv1, opcode, payload }) = self.take_frame()? {
                match opcode {
                    0x9 => {
                        self.send_frame(0xA, &payload).await?;
                        continue;
                    }
                    0xA => continue,
                    0x8 => {
                        let close =
                            (payload.len() >= 2).then(|| (u16::from_be_bytes([payload[0], payload[1]]), String::from_utf8_lossy(&payload[2..]).into_owned()));
                        if !self.closed {
                            self.closed = true;
                            let _ = self.send_frame(0x8, &payload[..payload.len().min(2)]).await;
                        }
                        return Ok(Some(Message::Close(close)));
                    }
                    0x1 | 0x2 => {
                        if self.partial.is_some() {
                            bail!("WebSocket: a new message started inside a fragmented one");
                        }
                        self.partial = Some((opcode, rsv1, payload));
                    }
                    0x0 => {
                        let Some((_, _, data)) = self.partial.as_mut() else { bail!("WebSocket: a continuation frame with nothing to continue") };
                        data.extend_from_slice(&payload);
                        if data.len() > MAX_MESSAGE {
                            bail!("WebSocket: message larger than {} MB", MAX_MESSAGE >> 20);
                        }
                    }
                    other => bail!("WebSocket: unknown opcode {other:#x}"),
                }
                if fin && let Some((op, compressed, data)) = self.partial.take() {
                    let data = if compressed { self.inflate(data)? } else { data };
                    return Ok(Some(if op == 0x1 {
                        Message::Text(String::from_utf8(data).context("WebSocket: text that isn't UTF-8")?)
                    } else {
                        Message::Binary(data)
                    }));
                }
                continue;
            }
            let mut chunk = [0u8; 16384];
            let n = match self.io.read(&mut chunk).await {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => 0,
                Err(e) => return Err(e.into()),
            };
            if n == 0 {
                return if self.closed { Ok(None) } else { Err(anyhow!("WebSocket: the connection dropped")) };
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// The next whole frame from the buffer, or None until more bytes come.
    fn take_frame(&mut self) -> Result<Option<Frame>> {
        let b = &self.buf;
        if b.len() < 2 {
            return Ok(None);
        }
        let (fin, rsv1, rsv23, opcode) = (b[0] & 0x80 != 0, b[0] & 0x40 != 0, b[0] & 0x30, b[0] & 0x0f);
        if rsv23 != 0 || (rsv1 && (self.inflate.is_none() || opcode == 0 || opcode >= 0x8)) {
            bail!("WebSocket: reserved bits set without an extension for them");
        }
        let masked = b[1] & 0x80 != 0;
        let (len, mut at) = match b[1] & 0x7f {
            126 if b.len() >= 4 => (u16::from_be_bytes([b[2], b[3]]) as usize, 4),
            127 if b.len() >= 10 => (u64::from_be_bytes(b[2..10].try_into().unwrap_or_default()) as usize, 10),
            126 | 127 => return Ok(None),
            n => (n as usize, 2),
        };
        if len > MAX_MESSAGE {
            bail!("WebSocket: frame larger than {} MB", MAX_MESSAGE >> 20);
        }
        let mask = if masked {
            if b.len() < at + 4 {
                return Ok(None);
            }
            let m = [b[at], b[at + 1], b[at + 2], b[at + 3]];
            at += 4;
            Some(m)
        } else {
            None
        };
        if b.len() < at + len {
            return Ok(None);
        }
        let mut payload = b[at..at + len].to_vec();
        if let Some(m) = mask {
            payload.iter_mut().enumerate().for_each(|(i, x)| *x ^= m[i % 4]);
        }
        self.buf.drain(..at + len);
        Ok(Some(Frame { fin, rsv1, opcode, payload }))
    }

    /// Inflate one compressed message (RFC 7692: raw DEFLATE ending in an empty stored block,
    /// whose four bytes the sender leaves off).
    fn inflate(&mut self, mut data: Vec<u8>) -> Result<Vec<u8>> {
        let inf = self.inflate.as_mut().context("WebSocket: a compressed message without permessage-deflate")?;
        data.extend_from_slice(&[0x00, 0x00, 0xff, 0xff]);
        let mut out = Vec::with_capacity(data.len() * 4);
        let start_in = inf.z.total_in();
        loop {
            let consumed = (inf.z.total_in() - start_in) as usize;
            if out.capacity() - out.len() < 4096 {
                out.reserve(out.len().max(16384));
            }
            let before = (inf.z.total_in(), inf.z.total_out());
            inf.z.decompress_vec(&data[consumed..], &mut out, flate2::FlushDecompress::Sync).context("WebSocket: bad compressed message")?;
            if out.len() > MAX_MESSAGE {
                bail!("WebSocket: message larger than {} MB once inflated", MAX_MESSAGE >> 20);
            }
            let done = (inf.z.total_in() - start_in) as usize >= data.len();
            if done && out.len() < out.capacity() {
                break;
            }
            if (inf.z.total_in(), inf.z.total_out()) == before {
                bail!("WebSocket: compressed message didn't inflate");
            }
        }
        if inf.reset {
            inf.z.reset(false);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_the_rfc_example() {
        // RFC 6455 section 1.3.
        assert_eq!(accept_for("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    /// Raw DEFLATE of `msgs` in one stream (context takeover), each message flushed and its
    /// trailing 00 00 ff ff left off, as a permessage-deflate sender does.
    fn deflate_messages(msgs: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut z = flate2::Compress::new(flate2::Compression::default(), false);
        msgs.iter()
            .map(|m| {
                let mut out = Vec::with_capacity(m.len() + 64);
                z.compress_vec(m, &mut out, flate2::FlushCompress::Sync).unwrap();
                assert!(out.ends_with(&[0, 0, 0xff, 0xff]));
                out.truncate(out.len() - 4);
                out
            })
            .collect()
    }

    fn frame(fin: bool, rsv1: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![(if fin { 0x80 } else { 0 }) | (if rsv1 { 0x40 } else { 0 }) | opcode];
        match payload.len() {
            n if n < 126 => f.push(n as u8),
            n => {
                f.push(126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
        }
        f.extend_from_slice(payload);
        f
    }

    /// A one-connection server: answers the handshake (with `ext` as its extensions header),
    /// writes `frames`, then reads whatever the client sends until it closes.
    async fn serve(ext: &'static str, frames: Vec<u8>) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://127.0.0.1:{}/v0/stream", l.local_addr().unwrap().port());
        let h = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut req = Vec::new();
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut c = [0u8; 1024];
                let n = s.read(&mut c).await.unwrap();
                req.extend_from_slice(&c[..n]);
            }
            let req = String::from_utf8(req).unwrap();
            let key = req.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Key: ")).unwrap().trim().to_string();
            assert!(req.contains("Sec-WebSocket-Protocol: mqtt"));
            let ext = if ext.is_empty() { String::new() } else { format!("Sec-WebSocket-Extensions: {ext}\r\n") };
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n{ext}\r\n",
                accept_for(&key)
            );
            s.write_all(resp.as_bytes()).await.unwrap();
            s.write_all(&frames).await.unwrap();
            let mut got = Vec::new();
            let _ = s.read_to_end(&mut got).await;
            got
        });
        (url, h)
    }

    #[tokio::test]
    async fn inflates_compressed_messages_across_frames() {
        let one = br#"{"MessageType":"PositionReport","MetaData":{"MMSI":367000001}}"#;
        let two = br#"{"MessageType":"PositionReport","MetaData":{"MMSI":367000002}}"#;
        let z = deflate_messages(&[one, two, b"plain"]);
        let mut frames = frame(true, true, 0x2, &z[0]);
        // The second message compressed and split over two frames, a ping between them.
        let (a, b) = z[1].split_at(z[1].len() / 2);
        frames.extend(frame(false, true, 0x2, a));
        frames.extend(frame(true, false, 0x9, b"hi"));
        frames.extend(frame(true, false, 0x0, b));
        // An uncompressed text message is still fine with deflate on.
        frames.extend(frame(true, false, 0x1, b"plain"));
        frames.extend(frame(true, false, 0x8, &[0x03, 0xe8]));
        let (url, server) = serve("permessage-deflate; client_max_window_bits=15", frames).await;
        let mut ws = connect(&url, Options { protocol: Some("mqtt"), deflate: true }).await.unwrap();
        assert!(ws.compressed());
        ws.send_text("subscribe").await.unwrap();
        assert_eq!(ws.next().await.unwrap(), Some(Message::Binary(one.to_vec())));
        assert_eq!(ws.next().await.unwrap(), Some(Message::Binary(two.to_vec())), "the window carries over between messages");
        assert_eq!(ws.next().await.unwrap(), Some(Message::Text("plain".into())));
        assert_eq!(ws.next().await.unwrap(), Some(Message::Close(Some((1000, String::new())))));
        ws.close().await.unwrap();
        // The server got the masked subscription, the pong and the closing frame.
        let got = server.await.unwrap();
        assert_eq!(got[0], 0x81);
        assert_eq!(got[1], 0x80 | 9, "client frames are masked");
        let m = &got[2..6];
        let text: Vec<u8> = got[6..15].iter().enumerate().map(|(i, b)| b ^ m[i % 4]).collect();
        assert_eq!(text, b"subscribe");
        assert_eq!(got[15], 0x8A, "a pong for the ping");
    }

    #[tokio::test]
    async fn resets_the_window_when_the_server_asks() {
        // With server_no_context_takeover each message is its own stream.
        let msgs: [&[u8]; 2] = [b"same words same words", b"same words same words"];
        let frames: Vec<u8> = msgs.iter().flat_map(|m| frame(true, true, 0x1, &deflate_messages(&[m])[0])).collect();
        let (url, _server) = serve("permessage-deflate; server_no_context_takeover", frames).await;
        let mut ws = connect(&url, Options { protocol: Some("mqtt"), deflate: true }).await.unwrap();
        for _ in 0..2 {
            assert_eq!(ws.next().await.unwrap(), Some(Message::Text("same words same words".into())));
        }
    }

    #[tokio::test]
    async fn refuses_what_it_did_not_ask_for() {
        // Compressed frames without the extension.
        let (url, _s) = serve("", frame(true, true, 0x2, b"xx")).await;
        let mut ws = connect(&url, Options { protocol: Some("mqtt"), deflate: false }).await.unwrap();
        assert!(!ws.compressed());
        assert!(ws.next().await.is_err());
        // An extension we didn't ask for.
        let (url, _s) = serve("x-webkit-deflate-frame", Vec::new()).await;
        assert!(connect(&url, Options { protocol: Some("mqtt"), deflate: true }).await.is_err());
    }
}
