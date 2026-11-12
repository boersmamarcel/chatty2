//! The browser's way out under the open-web policy (AGE-1290).
//!
//! The navigation policy checks a URL's host with the SSRF guard, but Chrome
//! then resolved the name again on its own. A name with a short TTL could
//! answer a public address to the check and `169.254.169.254` or a LAN
//! address to Chrome (DNS rebinding), and subresources (images, `fetch`,
//! forms) were never checked at all.
//!
//! Chrome now runs with `--proxy-server` pointing at [`GuardedProxy`], a
//! small HTTP proxy in this process. Every request Chrome makes to a
//! non-loopback host — navigations, redirects, subresources, WebSockets —
//! comes here by name. The proxy resolves the name once through
//! [`GuardedResolver`] with the browser's address policy and connects to
//! exactly the addresses it checked, so the address checked is the address
//! used, the way `fetch` works (AGE-537).
//!
//! - `CONNECT host:port` (HTTPS, WSS): checked, then a byte tunnel.
//! - `GET http://host/...` (plain HTTP): checked, the request is rewritten
//!   to origin form with `Connection: close`, so a kept-alive client
//!   connection is never reused for a second host.
//! - A refused host gets `403` with the reason.
//!
//! Loopback is not proxied: Chrome sends `localhost` and `127.0.0.0/8`
//! straight to the machine, as Lane A intends.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::services::ssrf_guard::{
    AddressPolicy, GuardedResolver, HostLookup, SystemLookup, is_blocked_hostname,
    is_link_local_ip, is_private_ip,
};

/// Largest request head the proxy reads before refusing.
const MAX_HEAD_BYTES: usize = 32 * 1024;
/// How long a client gets to send its request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an upstream connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The browser's address policy: never link-local (cloud metadata) or a
/// denylisted name; private ranges only with the workspace's
/// `allow_private_network_access` (AGE-459).
pub(crate) fn browser_policy(allow_private_network_access: bool) -> AddressPolicy {
    Arc::new(move |host: &str, ip: IpAddr| {
        if is_link_local_ip(&ip) || is_blocked_hostname(host) {
            return false;
        }
        allow_private_network_access || !is_private_ip(&ip)
    })
}

/// A running proxy; stops when dropped.
pub(crate) struct GuardedProxy {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl GuardedProxy {
    /// Start a proxy on a free loopback port.
    pub(crate) async fn start(allow_private_network_access: bool) -> std::io::Result<Self> {
        Self::start_with(
            Arc::new(SystemLookup),
            browser_policy(allow_private_network_access),
        )
        .await
    }

    pub(crate) async fn start_with(
        lookup: Arc<dyn HostLookup>,
        policy: AddressPolicy,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let guard = Arc::new(Guard {
            resolver: GuardedResolver::new(lookup, policy.clone()),
            policy,
        });
        let task = tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    continue;
                };
                let guard = Arc::clone(&guard);
                tokio::spawn(async move {
                    if let Err(why) = handle(client, &guard).await {
                        debug!(%why, "browser proxy: connection ended");
                    }
                });
            }
        });
        Ok(Self { addr, task })
    }

    /// The `--proxy-server` value for Chrome.
    pub(crate) fn proxy_server_arg(&self) -> String {
        format!("http://{}", self.addr)
    }

    #[cfg(test)]
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for GuardedProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Guard {
    resolver: GuardedResolver,
    policy: AddressPolicy,
}

impl Guard {
    /// Connect to `host:port` through checked addresses only.
    async fn connect(&self, host: &str, port: u16) -> Result<TcpStream, String> {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let addrs = match bare.parse::<IpAddr>() {
            Ok(ip) => {
                if !(self.policy)(bare, ip) {
                    warn!(%ip, "browser proxy: refused an address");
                    return Err(format!(
                        "{ip} is a private/internal address, refusing (SSRF protection)"
                    ));
                }
                vec![ip]
            }
            Err(_) => self.resolver.resolve_checked(bare).await?,
        };
        let mut last = String::from("no address");
        for ip in addrs {
            let target = SocketAddr::new(ip, port);
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target)).await {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(e)) => last = e.to_string(),
                Err(_) => last = "connect timed out".into(),
            }
        }
        Err(format!("could not connect to {host}:{port}: {last}"))
    }
}

async fn handle(mut client: TcpStream, guard: &Guard) -> Result<(), String> {
    let (head, rest) = tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client))
        .await
        .map_err(|_| "request head timed out".to_string())??;
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (method, target, version) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or("HTTP/1.1"),
    );

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, 443)?;
        let mut upstream = match guard.connect(&host, port).await {
            Ok(stream) => stream,
            Err(why) => return refuse(&mut client, &why).await,
        };
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(|e| e.to_string())?;
        upstream.write_all(&rest).await.map_err(|e| e.to_string())?;
        tokio::io::copy_bidirectional(&mut client, &mut upstream)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(());
    }

    // Plain HTTP in absolute form: `GET http://host[:port]/path HTTP/1.1`.
    let Ok(url) = reqwest::Url::parse(target) else {
        return refuse(&mut client, "the proxy only forwards absolute http:// URLs").await;
    };
    if url.scheme() != "http" {
        return refuse(&mut client, "the proxy only forwards http:// requests").await;
    }
    let Some(host) = url.host_str().map(str::to_string) else {
        return refuse(&mut client, "the URL has no host").await;
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let mut upstream = match guard.connect(&host, port).await {
        Ok(stream) => stream,
        Err(why) => return refuse(&mut client, &why).await,
    };
    let path = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    };
    let mut out = format!("{method} {path} {version}\r\n");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let name = line.split(':').next().unwrap_or_default().trim();
        if [
            "proxy-connection",
            "connection",
            "keep-alive",
            "proxy-authorization",
        ]
        .iter()
        .any(|h| name.eq_ignore_ascii_case(h))
        {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    upstream
        .write_all(out.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    upstream.write_all(&rest).await.map_err(|e| e.to_string())?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Read up to the end of the request head; returns the head (without the
/// blank line) and any bytes read past it.
async fn read_head(client: &mut TcpStream) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            buf.truncate(end);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err("request head too large".into());
        }
        let n = client.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("client closed before the request head ended".into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn split_host_port(target: &str, default_port: u16) -> Result<(String, u16), String> {
    // `[v6]:port`, `host:port`, or a bare host.
    if let Some(rest) = target.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| format!("bad CONNECT target {target}"))?;
        let port = after
            .strip_prefix(':')
            .map(|p| {
                p.parse::<u16>()
                    .map_err(|_| format!("bad port in {target}"))
            })
            .transpose()?
            .unwrap_or(default_port);
        return Ok((host.to_string(), port));
    }
    match target.rsplit_once(':') {
        Some((host, port)) => Ok((
            host.to_string(),
            port.parse().map_err(|_| format!("bad port in {target}"))?,
        )),
        None => Ok((target.to_string(), default_port)),
    }
}

async fn refuse(client: &mut TcpStream, why: &str) -> Result<(), String> {
    let body = format!("Blocked by Chatty's browser network policy: {why}\n");
    let response = format!(
        "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = client.write_all(response.as_bytes()).await;
    Err(why.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;

    /// Resolves every name to one fixed address, like a rebinding DNS
    /// answer for the connection the browser would make.
    struct Fixed(IpAddr);
    impl HostLookup for Fixed {
        fn lookup(
            &self,
            _host: &str,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>> {
            let ip = self.0;
            Box::pin(async move { Ok(vec![ip]) })
        }
    }

    async fn send(proxy: SocketAddr, request: &str) -> String {
        let mut stream = TcpStream::connect(proxy).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    /// A name that resolves to the metadata address is refused, for CONNECT
    /// and plain HTTP alike, and so is a link-local IP literal.
    #[tokio::test]
    async fn names_resolving_to_metadata_are_refused() {
        let proxy = GuardedProxy::start_with(
            Arc::new(Fixed("169.254.169.254".parse().unwrap())),
            browser_policy(true),
        )
        .await
        .unwrap();
        let connect = send(
            proxy.addr(),
            "CONNECT rebind.example:443 HTTP/1.1\r\nHost: rebind.example:443\r\n\r\n",
        )
        .await;
        assert!(connect.starts_with("HTTP/1.1 403"), "{connect}");
        let get = send(
            proxy.addr(),
            "GET http://rebind.example/latest/meta-data HTTP/1.1\r\nHost: rebind.example\r\n\r\n",
        )
        .await;
        assert!(get.starts_with("HTTP/1.1 403"), "{get}");
        let literal = send(proxy.addr(), "CONNECT 169.254.169.254:80 HTTP/1.1\r\n\r\n").await;
        assert!(literal.starts_with("HTTP/1.1 403"), "{literal}");
    }

    /// Without the private-network opt-in a LAN answer is refused; with it,
    /// the request reaches the checked address (a local test server).
    #[tokio::test]
    async fn private_answers_follow_the_workspace_toggle() {
        let server = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = server.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = s.read(&mut buf).await;
                    let _ = s
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await;
                });
            }
        });
        let request = format!(
            "GET http://lan.example:{port}/ HTTP/1.1\r\nHost: lan.example\r\nProxy-Connection: keep-alive\r\n\r\n"
        );

        let closed = GuardedProxy::start_with(
            Arc::new(Fixed("127.0.0.1".parse().unwrap())),
            browser_policy(false),
        )
        .await
        .unwrap();
        let refused = send(closed.addr(), &request).await;
        assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");

        let open = GuardedProxy::start_with(
            Arc::new(Fixed("127.0.0.1".parse().unwrap())),
            browser_policy(true),
        )
        .await
        .unwrap();
        let ok = send(open.addr(), &request).await;
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.ends_with("ok"), "{ok}");
    }

    #[test]
    fn connect_targets_parse() {
        assert_eq!(
            split_host_port("example.com:8443", 443).unwrap(),
            ("example.com".to_string(), 8443)
        );
        assert_eq!(
            split_host_port("[::1]:443", 443).unwrap(),
            ("::1".to_string(), 443)
        );
        assert_eq!(
            split_host_port("example.com", 443).unwrap(),
            ("example.com".to_string(), 443)
        );
    }
}
