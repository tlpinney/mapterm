# Network and proxies

Everything mapterm fetches (vector tiles, imagery, styles, search, and the live layers'
feeds and WebSocket streams) goes through one place, so one setting sends all of it
through a proxy:

```
mapterm --proxy socks5h://127.0.0.1:1080 ...
MAPTERM_PROXY=socks5h://127.0.0.1:1080 mapterm ...
```

| URL | What it is |
|---|---|
| `socks5h://HOST:PORT` | SOCKS5; the proxy looks up host names, so nothing about where you're going leaves your machine except to the proxy. Usually the one to use. |
| `socks5://HOST:PORT` | SOCKS5; host names are looked up on your machine first. |
| `http://HOST:PORT` | An HTTP proxy (`CONNECT` for HTTPS and WebSockets). |

A user name and password go in the URL, percent-encoded where needed:
`socks5h://user:p%40ss@host:1080`. mapterm never prints the password; messages show it
as `***`. Without a port, SOCKS means 1080 and HTTP 80.

Local addresses (`localhost`, `127.0.0.1`, `::1`) always go direct, so local tile servers
and files keep working.

When a proxy is asked for, mapterm uses it or fails: a proxy URL that doesn't parse is an
error, never a quiet fallback to connecting directly.

Some uses: a network that only allows traffic through a proxy; Tor (`socks5h://127.0.0.1:9050`);
a VPN provider's SOCKS proxy, or one running in a container, to reach a service that
your own address can't.

## In your own app

```rust
mapterm::proxy::set("socks5h://127.0.0.1:1080")?; // before the first request
```

or set `MAPTERM_PROXY`. `mapterm::source::http_client()` and `mapterm::ws::connect` then
use it.
