//! Shared SSRF (server-side request forgery) host validation.
//!
//! `fetch_tool` (agent-chosen URLs on the open internet) and the browser's
//! internet-enabled navigation policy both need to refuse the same private,
//! loopback, link-local, and cloud-metadata targets. One denylist here, so
//! the two can't drift apart the way two hand-copied lists would.
//!
//! A check is not enough on its own: a client that resolves the name again
//! to connect can be sent somewhere else (DNS rebinding). Clients that
//! connect use [`GuardedResolver`], which checks the addresses it hands the
//! connector — one lookup for both (AGE-537, AGE-767).

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use tracing::warn;

/// Validate that `url`'s host is not a private, internal, loopback, or
/// cloud-metadata target. Resolves hostnames to catch DNS rebinding /
/// split-horizon attacks where a public name resolves to a private IP.
///
/// Fails closed: a name that does not resolve, or resolves to nothing, is
/// refused (AGE-537). This is a check only — a request made afterwards
/// resolves again, so a caller that connects must use a client built with
/// [`GuardedResolver`], which checks the addresses it connects to.
pub fn check_public_host(url: &str) -> Result<(), String> {
    check_host_with(url, false, &system_lookup_blocking)
}

/// Same as [`check_public_host`], except when `allow_private_network_access`
/// is true, private/internal ranges (RFC-1918, loopback-as-IP, CGN,
/// benchmarking, multicast, "this network") are not rejected.
///
/// The link-local range `169.254.0.0/16` — which includes the cloud-metadata
/// address `169.254.169.254` — is never bypassed, even with the flag on:
/// that carve-out is deliberate (AGE-459) and applied before the flag is
/// consulted at all, in [`is_blocked_ip`]. Only the browser's per-workspace
/// toggle should ever pass `true` here; `fetch_tool` always calls
/// [`check_public_host`].
pub fn check_public_host_with_bypass(
    url: &str,
    allow_private_network_access: bool,
) -> Result<(), String> {
    check_host_with(url, allow_private_network_access, &system_lookup_blocking)
}

/// The part of [`check_public_host`] that needs no DNS: the scheme-free
/// URL parses, its host is not a denylisted name, and an IP-literal host is
/// public. A hostname passes here and is checked where it is resolved, by
/// the [`GuardedResolver`] of the client that connects to it.
pub fn check_public_url_without_lookup(url: &str) -> Result<(), String> {
    let host = parse_host(url)?;
    if is_blocked_hostname(&host) {
        return Err(blocked_hostname_error(&host));
    }
    if let Some(ip) = ip_literal(&host)
        && !public_only(&host, ip)
    {
        return Err(blocked_ip_error(ip));
    }
    Ok(())
}

fn parse_host(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL {url}: {e}"))?;
    parsed
        .host_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{url} has no host"))
}

/// The host as an IP address, if it is an IP literal. `host_str()` returns
/// IPv6 in brackets ("[::1]"); `IpAddr` wants it without.
fn ip_literal(host: &str) -> Option<IpAddr> {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .ok()
}

fn blocked_hostname_error(host: &str) -> String {
    format!("requests to '{host}' are blocked for security (SSRF protection)")
}

fn blocked_ip_error(ip: IpAddr) -> String {
    format!("requests to private/internal IP '{ip}' are blocked for security (SSRF protection)")
}

/// A blocking system lookup, for the synchronous checks above.
fn system_lookup_blocking(host: &str) -> std::io::Result<Vec<IpAddr>> {
    std::net::ToSocketAddrs::to_socket_addrs(&(host, 80))
        .map(|addrs| addrs.map(|addr| addr.ip()).collect())
}

fn check_host_with(
    url: &str,
    allow_private_network_access: bool,
    lookup: &dyn Fn(&str) -> std::io::Result<Vec<IpAddr>>,
) -> Result<(), String> {
    let host = parse_host(url)?;

    if is_blocked_hostname(&host) {
        return Err(blocked_hostname_error(&host));
    }

    if let Some(ip) = ip_literal(&host) {
        if is_blocked_ip(&ip, allow_private_network_access) {
            return Err(blocked_ip_error(ip));
        }
        return Ok(());
    }

    // Hostname: resolve and check every resolved address. A name that does
    // not resolve is refused, never waved through (AGE-537).
    let addrs = lookup(&host)
        .map_err(|e| format!("'{host}' could not be resolved, refusing (SSRF protection): {e}"))?;
    if addrs.is_empty() {
        return Err(format!(
            "'{host}' resolved to no address, refusing (SSRF protection)"
        ));
    }
    for ip in addrs {
        if is_blocked_ip(&ip, allow_private_network_access) {
            warn!(host = %host, resolved_ip = %ip, "Blocked DNS-resolved private IP");
            return Err(format!(
                "'{host}' resolves to private/internal IP {ip} (SSRF protection)"
            ));
        }
    }

    Ok(())
}

// ── Resolve once, connect to what was checked ──────────────────────────────

/// Which resolved addresses a [`GuardedResolver`] lets a client connect to,
/// given the host name they were resolved for. `true` admits the address.
///
/// An `Arc<dyn Fn>` rather than a bare `fn` pointer so a policy can close
/// over a per-agent setting (AGE-806's `allow_private_network`), not just
/// dispatch on the arguments.
pub type AddressPolicy = Arc<dyn Fn(&str, IpAddr) -> bool + Send + Sync>;

/// The open-web policy (`fetch`): no denylisted name, no private, loopback,
/// link-local or otherwise reserved address.
pub fn public_only(host: &str, ip: IpAddr) -> bool {
    !is_blocked_hostname(host) && !is_private_ip(&ip)
}

/// The A2A edge policy (ADR-0021 step 0, AGE-767) for a user-configured
/// remote agent.
///
/// - Link-local, including cloud metadata, is never reachable.
/// - `localhost` must resolve to loopback and nothing else.
/// - An IP literal is the address the user configured; it needs no DNS and
///   cannot be rebound, so it is admitted (TLS is enforced separately by
///   `hive_client::ensure_secure_url`).
/// - Any other name must resolve to public addresses only, so a public
///   agent's DNS can never be turned into a path to this machine or its
///   network. An agent on a private network is configured by its IP.
pub fn a2a_peer(host: &str, ip: IpAddr) -> bool {
    if is_link_local_ip(&ip) {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return ip.is_loopback();
    }
    if ip_literal(host).is_some() {
        return true;
    }
    public_only(host, ip)
}

/// [`a2a_peer`], but for an agent with AGE-806's per-agent
/// `allow_private_network` opt-in turned on: a *name* that resolves to a
/// private range (RFC-1918, CGN/Tailscale `100.64.0.0/10`, ULA) is admitted
/// too, with the same bypass semantics as the browser's per-workspace
/// toggle (`check_public_host_with_bypass`, AGE-459) — not just a configured
/// IP literal, which [`a2a_peer`] already admits unconditionally.
///
/// Unchanged, flag or not:
/// - Link-local, including cloud metadata (`169.254.0.0/16`, `fe80::/10`),
///   is never reachable — checked first, before the flag is ever consulted.
/// - `localhost` must resolve to loopback and nothing else.
/// - The hostname denylist (`is_blocked_hostname`: `.internal`, `.local`,
///   the GCP metadata name) still applies.
///
/// With `allow_private_network` false this is exactly [`a2a_peer`].
pub fn a2a_peer_with_bypass(allow_private_network: bool) -> AddressPolicy {
    Arc::new(move |host: &str, ip: IpAddr| {
        if is_link_local_ip(&ip) {
            return false;
        }
        if host.eq_ignore_ascii_case("localhost") {
            return ip.is_loopback();
        }
        if ip_literal(host).is_some() {
            return true;
        }
        if allow_private_network {
            !is_blocked_hostname(host)
        } else {
            public_only(host, ip)
        }
    })
}

/// The IP-literal half of [`a2a_peer`], for the URL itself: reqwest never
/// hands a literal to the resolver, so the resolver cannot refuse it.
pub fn check_a2a_url_without_lookup(url: &str) -> Result<(), String> {
    let host = parse_host(url)?;
    match ip_literal(&host) {
        Some(ip) if !a2a_peer(&host, ip) => Err(blocked_ip_error(ip)),
        _ => Ok(()),
    }
}

/// A DNS lookup the [`GuardedResolver`] runs exactly once per connection.
/// Tests substitute a stub; production uses [`SystemLookup`].
pub trait HostLookup: Send + Sync {
    fn lookup(
        &self,
        host: &str,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>;
}

/// The system resolver, off the async runtime's worker threads.
pub struct SystemLookup;

impl HostLookup for SystemLookup {
    fn lookup(
        &self,
        host: &str,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>> {
        let host = host.to_string();
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .map(|addr| addr.ip())
                .collect())
        })
    }
}

/// A reqwest DNS resolver that checks what it resolves and hands the client
/// exactly those addresses to connect to.
///
/// The check and the connection share one lookup, so a name cannot pass
/// with one answer and be connected to under another (DNS rebinding, the
/// TOCTOU of a separate pre-check). A lookup error, an empty answer, or any
/// address the policy refuses fails the connection: fail closed (AGE-537).
///
/// Only names reach a resolver; IP literals must be checked up front. A
/// client using this must also disable proxies (`no_proxy`), or the name
/// resolved would be the proxy's, not the target's.
pub struct GuardedResolver {
    lookup: Arc<dyn HostLookup>,
    policy: AddressPolicy,
}

impl GuardedResolver {
    pub fn new(lookup: Arc<dyn HostLookup>, policy: AddressPolicy) -> Self {
        Self { lookup, policy }
    }

    /// Resolve `host` once and admit or refuse the whole answer.
    pub async fn resolve_checked(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let addrs = self.lookup.lookup(host).await.map_err(|e| {
            format!("'{host}' could not be resolved, refusing (SSRF protection): {e}")
        })?;
        if addrs.is_empty() {
            return Err(format!(
                "'{host}' resolved to no address, refusing (SSRF protection)"
            ));
        }
        if let Some(ip) = addrs.iter().find(|ip| !(self.policy)(host, **ip)) {
            warn!(host = %host, resolved_ip = %ip, "Refused to connect to a resolved address");
            return Err(format!(
                "'{host}' resolves to private/internal IP {ip}, refusing (SSRF protection)"
            ));
        }
        Ok(addrs)
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let lookup = Arc::clone(&self.lookup);
        let policy = Arc::clone(&self.policy);
        let host = name.as_str().to_string();
        Box::pin(async move {
            let guard = GuardedResolver { lookup, policy };
            let addrs = guard.resolve_checked(&host).await?;
            // The port is the connector's to set; 0 is a placeholder.
            let addrs: reqwest::dns::Addrs =
                Box::new(addrs.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// Whether `ip` should be rejected, given whether the caller has opted into
/// reaching the rest of the private-network space.
///
/// Link-local (including cloud metadata) is checked first and always blocks,
/// regardless of `allow_private_network_access` — see [`check_public_host_with_bypass`].
fn is_blocked_ip(ip: &IpAddr, allow_private_network_access: bool) -> bool {
    if is_link_local_ip(ip) {
        return true;
    }
    if allow_private_network_access {
        return false;
    }
    is_private_ip(ip)
}

/// Check if a hostname string is a known-blocked name (case-insensitive).
pub fn is_blocked_hostname(host: &str) -> bool {
    let h = host.to_lowercase();
    h == "localhost"
        || h == "metadata.google.internal"  // GCP metadata
        || h.ends_with(".internal")
        || h.ends_with(".local")
}

/// Check if an IP address is in the link-local range: `169.254.0.0/16` for
/// IPv4 (which includes the AWS/GCP/Azure cloud-metadata address
/// `169.254.169.254`) and `fe80::/10` for IPv6, including either reached
/// through an IPv4-mapped IPv6 address. Kept as its own predicate, separate
/// from [`is_private_ip`], because AGE-459's workspace-level toggle carves
/// this range out explicitly rather than lumping it in with "private".
pub fn is_link_local_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            octets[0] == 169 && octets[1] == 254
        }
        IpAddr::V6(v6) => {
            (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .map(|v4| is_link_local_ip(&IpAddr::V4(v4)))
                    .unwrap_or(false)
        }
    }
}

/// Check if an IP address belongs to a private, loopback, link-local, or otherwise
/// reserved network range that should not be reachable from an open-web request.
pub fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 127.0.0.0/8 — loopback
            octets[0] == 127
            // 10.0.0.0/8 — RFC-1918 private
            || octets[0] == 10
            // 172.16.0.0/12 — RFC-1918 private
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            // 192.168.0.0/16 — RFC-1918 private
            || (octets[0] == 192 && octets[1] == 168)
            // 169.254.0.0/16 — link-local (includes AWS/GCP/Azure metadata at 169.254.169.254)
            || (octets[0] == 169 && octets[1] == 254)
            // 0.0.0.0/8 — "this" network
            || octets[0] == 0
            // 100.64.0.0/10 — shared address space (CGN, often internal)
            || (octets[0] == 100 && (64..=127).contains(&octets[1]))
            // 198.18.0.0/15 — benchmarking
            || (octets[0] == 198 && (18..=19).contains(&octets[1]))
            // 224.0.0.0/4 — multicast
            || octets[0] >= 224
        }
        IpAddr::V6(v6) => {
            // ::1 — loopback
            v6.is_loopback()
            // fe80::/10 — link-local
            || (v6.segments()[0] & 0xffc0) == 0xfe80
            // fc00::/7 — unique local (ULA, RFC-4193)
            || (v6.segments()[0] & 0xfe00) == 0xfc00
            // :: — unspecified
            || v6.is_unspecified()
            // ::ffff:x.x.x.x — IPv4-mapped, check the embedded v4 address
            || v6.to_ipv4_mapped().map(|v4| is_private_ip(&IpAddr::V4(v4))).unwrap_or(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_private_ip_loopback() {
        assert!(is_private_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_private_ip(&"127.0.0.2".parse().unwrap()));
        assert!(is_private_ip(&"::1".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_rfc1918() {
        assert!(is_private_ip(&"10.0.0.1".parse().unwrap()));
        assert!(is_private_ip(&"10.255.255.255".parse().unwrap()));
        assert!(is_private_ip(&"172.16.0.1".parse().unwrap()));
        assert!(is_private_ip(&"172.31.255.255".parse().unwrap()));
        assert!(is_private_ip(&"192.168.0.1".parse().unwrap()));
        assert!(is_private_ip(&"192.168.255.255".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_link_local_and_metadata() {
        // AWS/GCP/Azure metadata endpoint
        assert!(is_private_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_private_ip(&"169.254.0.1".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_public() {
        assert!(!is_private_ip(&"8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip(&"1.1.1.1".parse().unwrap()));
        assert!(!is_private_ip(&"93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_other_reserved() {
        assert!(is_private_ip(&"0.0.0.0".parse().unwrap()));
        assert!(is_private_ip(&"100.64.0.1".parse().unwrap())); // CGN
        assert!(is_private_ip(&"224.0.0.1".parse().unwrap())); // multicast
        assert!(is_private_ip(&"255.255.255.255".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_v6() {
        // ULA
        assert!(is_private_ip(&"fd00::1".parse().unwrap()));
        // Link-local
        assert!(is_private_ip(&"fe80::1".parse().unwrap()));
        // Unspecified
        assert!(is_private_ip(&"::".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_v4_mapped_v6() {
        // ::ffff:127.0.0.1 should be blocked
        assert!(is_private_ip(&"::ffff:127.0.0.1".parse().unwrap()));
        // ::ffff:169.254.169.254 (metadata via v6)
        assert!(is_private_ip(&"::ffff:169.254.169.254".parse().unwrap()));
        // ::ffff:8.8.8.8 should be allowed
        assert!(!is_private_ip(&"::ffff:8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn blocked_hostname() {
        assert!(is_blocked_hostname("localhost"));
        assert!(is_blocked_hostname("LOCALHOST"));
        assert!(is_blocked_hostname("metadata.google.internal"));
        assert!(is_blocked_hostname("foo.internal"));
        assert!(is_blocked_hostname("printer.local"));
        assert!(!is_blocked_hostname("example.com"));
        assert!(!is_blocked_hostname("my-internal-api.com")); // "internal" in domain name is fine
    }

    #[test]
    fn check_public_host_blocks_private() {
        assert!(check_public_host("http://127.0.0.1/secret").is_err());
        assert!(check_public_host("http://localhost:8080/admin").is_err());
        assert!(check_public_host("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(check_public_host("http://10.0.0.1/internal").is_err());
        assert!(check_public_host("http://192.168.1.1/router").is_err());
        assert!(check_public_host("http://172.16.0.5/service").is_err());
        assert!(check_public_host("http://[::1]/secret").is_err());
        assert!(check_public_host("http://metadata.google.internal/computeMetadata/v1/").is_err());
    }

    #[test]
    fn check_public_host_allows_public() {
        let public = |_: &str| Ok(vec!["93.184.216.34".parse().unwrap()]);
        assert!(check_host_with("https://example.com", false, &public).is_ok());
        assert!(check_host_with("https://docs.rs/rig-core/latest", false, &public).is_ok());
    }

    /// AGE-537: a name the guard cannot resolve is refused, not let through.
    #[test]
    fn check_public_host_fails_closed_on_resolution_error() {
        let failing = |_: &str| Err(std::io::Error::other("stub: no such host"));
        let err = check_host_with("https://example.com", false, &failing).unwrap_err();
        assert!(err.contains("could not be resolved"), "{err}");

        let empty = |_: &str| Ok(Vec::new());
        let err = check_host_with("https://example.com", false, &empty).unwrap_err();
        assert!(err.contains("no address"), "{err}");
    }

    #[test]
    fn check_public_host_refuses_a_name_resolving_private() {
        let private = |_: &str| Ok(vec!["10.0.0.7".parse().unwrap()]);
        assert!(check_host_with("https://example.com", false, &private).is_err());
    }

    #[test]
    fn check_without_lookup_refuses_literals_and_names_but_not_hostnames() {
        assert!(check_public_url_without_lookup("http://127.0.0.1/").is_err());
        assert!(check_public_url_without_lookup("http://[::1]/").is_err());
        assert!(check_public_url_without_lookup("http://localhost:8080/").is_err());
        assert!(check_public_url_without_lookup("http://foo.internal/").is_err());
        assert!(check_public_url_without_lookup("https://8.8.8.8/").is_ok());
        // A hostname is left to the resolver that connects to it.
        assert!(check_public_url_without_lookup("https://example.com/").is_ok());
    }

    struct Stub(Vec<std::io::Result<Vec<IpAddr>>>, std::sync::Mutex<usize>);

    impl HostLookup for Stub {
        fn lookup(
            &self,
            _host: &str,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>> {
            let mut calls = self.1.lock().unwrap();
            let answer = match &self.0[*calls] {
                Ok(addrs) => Ok(addrs.clone()),
                Err(e) => Err(std::io::Error::other(e.to_string())),
            };
            *calls += 1;
            Box::pin(async move { answer })
        }
    }

    fn resolver(
        answers: Vec<std::io::Result<Vec<IpAddr>>>,
        policy: AddressPolicy,
    ) -> GuardedResolver {
        GuardedResolver::new(Arc::new(Stub(answers, std::sync::Mutex::new(0))), policy)
    }

    #[tokio::test]
    async fn guarded_resolver_admits_public_and_refuses_private_or_failed() {
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let guard = resolver(
            vec![
                Ok(vec![public]),
                Ok(vec![public, "127.0.0.1".parse().unwrap()]),
                Err(std::io::Error::other("stub")),
                Ok(vec![]),
            ],
            Arc::new(public_only),
        );
        assert_eq!(
            guard.resolve_checked("example.com").await.unwrap(),
            vec![public]
        );
        // One private address in the answer refuses the whole answer.
        assert!(guard.resolve_checked("example.com").await.is_err());
        assert!(guard.resolve_checked("example.com").await.is_err());
        assert!(guard.resolve_checked("example.com").await.is_err());
    }

    #[test]
    fn a2a_peer_policy() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // A configured literal is the user's choice, except link-local.
        assert!(a2a_peer("10.0.0.5", ip("10.0.0.5")));
        assert!(a2a_peer("127.0.0.1", ip("127.0.0.1")));
        assert!(!a2a_peer("169.254.169.254", ip("169.254.169.254")));
        // localhost means loopback and nothing else.
        assert!(a2a_peer("localhost", ip("127.0.0.1")));
        assert!(a2a_peer("localhost", ip("::1")));
        assert!(!a2a_peer("localhost", ip("10.0.0.5")));
        // Any other name must stay public.
        assert!(a2a_peer("agent.example.com", ip("93.184.216.34")));
        assert!(!a2a_peer("agent.example.com", ip("127.0.0.1")));
        assert!(!a2a_peer("agent.example.com", ip("192.168.1.9")));

        assert!(check_a2a_url_without_lookup("http://169.254.169.254/").is_err());
        assert!(check_a2a_url_without_lookup("http://127.0.0.1:8420/a2a").is_ok());
        assert!(check_a2a_url_without_lookup("https://agent.example.com/").is_ok());
    }

    /// AGE-806: with the opt-in off, `a2a_peer_with_bypass(false)` matches
    /// `a2a_peer` exactly — an agent without the flag keeps today's
    /// behaviour, including refusing a name that resolves private.
    #[test]
    fn a2a_peer_with_bypass_off_matches_a2a_peer() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let policy = a2a_peer_with_bypass(false);
        for (host, addr) in [
            ("10.0.0.5", "10.0.0.5"),
            ("127.0.0.1", "127.0.0.1"),
            ("169.254.169.254", "169.254.169.254"),
            ("localhost", "127.0.0.1"),
            ("localhost", "::1"),
            ("localhost", "10.0.0.5"),
            ("agent.example.com", "93.184.216.34"),
            ("agent.example.com", "127.0.0.1"),
            ("agent.example.com", "192.168.1.9"),
        ] {
            assert_eq!(
                policy(host, ip(addr)),
                a2a_peer(host, ip(addr)),
                "{host} / {addr}"
            );
        }
    }

    /// AGE-806: with the opt-in on, a *name* resolving to a LAN or Tailscale
    /// (CGN) address is admitted — the gap the issue closes.
    #[test]
    fn a2a_peer_with_bypass_on_admits_lan_and_tailscale_names() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let policy = a2a_peer_with_bypass(true);
        assert!(policy("agent.lan", ip("192.168.1.9")));
        assert!(policy("agent.lan", ip("10.0.0.5")));
        assert!(policy("agent.lan", ip("172.16.0.5")));
        // Tailscale's CGNAT range.
        assert!(policy("worker.tailnet", ip("100.64.1.2")));
        // IPv6 ULA.
        assert!(policy("agent.lan", ip("fd7a:115c:a1e0::1")));
        assert!(policy("agent.lan", ip("fd00::1")));
    }

    /// AGE-806: cloud metadata and IPv6 link-local stay refused even with
    /// the opt-in on.
    #[test]
    fn a2a_peer_with_bypass_on_still_refuses_link_local_metadata() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let policy = a2a_peer_with_bypass(true);
        assert!(!policy("agent.lan", ip("169.254.169.254")));
        assert!(!policy("agent.lan", ip("169.254.1.1")));
        assert!(!policy("agent.lan", ip("fe80::1")));
        // localhost still means loopback and nothing else, flag or not.
        assert!(!policy("localhost", ip("10.0.0.5")));
    }

    /// AGE-806: the hostname denylist (`.internal`, `.local`, the GCP
    /// metadata name) is unaffected by the opt-in.
    #[test]
    fn a2a_peer_with_bypass_on_still_blocks_denylisted_hostnames() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let policy = a2a_peer_with_bypass(true);
        assert!(!policy("printer.local", ip("192.168.1.9")));
        assert!(!policy("metadata.google.internal", ip("169.254.169.254")));
    }

    #[test]
    fn is_link_local_ip_covers_metadata_range() {
        assert!(is_link_local_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_link_local_ip(&"169.254.0.1".parse().unwrap()));
        assert!(is_link_local_ip(&"fe80::1".parse().unwrap()));
        assert!(is_link_local_ip(&"::ffff:169.254.169.254".parse().unwrap()));
        assert!(!is_link_local_ip(&"10.0.0.1".parse().unwrap()));
        assert!(!is_link_local_ip(&"192.168.1.1".parse().unwrap()));
        assert!(!is_link_local_ip(&"8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn check_public_host_with_bypass_off_matches_check_public_host() {
        // AGE-459: with the bypass off, behavior is unchanged from
        // `check_public_host` — every RFC-1918 / loopback-IP / metadata
        // target is still rejected.
        for url in [
            "http://10.0.0.1/internal",
            "http://192.168.1.1/router",
            "http://172.16.0.5/service",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            assert!(check_public_host_with_bypass(url, false).is_err());
        }
    }

    #[test]
    fn check_public_host_with_bypass_on_allows_private_ranges() {
        for url in [
            "http://10.0.0.1/internal",
            "http://192.168.1.1/router",
            "http://172.16.0.5/service",
            "http://100.64.0.1/cgn",
        ] {
            assert!(
                check_public_host_with_bypass(url, true).is_ok(),
                "{url} should be reachable with the toggle on"
            );
        }
    }

    #[test]
    fn check_public_host_with_bypass_on_still_blocks_link_local_metadata() {
        // AGE-459: the toggle never reaches the cloud-metadata range,
        // carved out ahead of the bypass check.
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.1.1/",
            "http://[fe80::1]/",
        ] {
            assert!(
                check_public_host_with_bypass(url, true).is_err(),
                "{url} must stay blocked even with the toggle on"
            );
        }
    }

    #[test]
    fn check_public_host_with_bypass_on_still_blocks_hostnames() {
        // The hostname denylist (localhost, .internal, .local, the GCP
        // metadata hostname) is a separate mechanism from the private-IP
        // check and is not affected by the toggle.
        for url in [
            "http://localhost:8080/admin",
            "http://foo.internal/",
            "http://printer.local/",
            "http://metadata.google.internal/computeMetadata/v1/",
        ] {
            assert!(check_public_host_with_bypass(url, true).is_err());
        }
    }

    #[test]
    fn check_public_host_with_bypass_on_still_allows_public_hosts() {
        let public = |_: &str| Ok(vec!["93.184.216.34".parse().unwrap()]);
        assert!(check_host_with("https://example.com", true, &public).is_ok());
    }
}
