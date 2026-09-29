//! TLS enforcement for remote endpoints the app is configured to reach
//! (SEC-16, AGE-756).
//!
//! Nothing used to stop a user from pointing a remote A2A agent, the Hive
//! registry, an MCP server, or an OpenAI-compatible provider at a plain
//! `http://` URL. Over a real network that sends prompts, answers, API keys
//! and bearer tokens in clear text. [`ensure_secure_url`] is the one gate
//! every one of those call sites goes through, both when the endpoint is
//! configured and when it is actually called.
//!
//! `https://` is always allowed. `http://` is allowed only for loopback and
//! for the private-LAN ranges (RFC-1918) that local model servers run on —
//! including the docker bridge address (`172.17.0.1`) a containerized vLLM
//! or Ollama listens on. Those ranges are not routable from outside this
//! machine's network, so `http://` there is not a wire eavesdropped by
//! anyone who isn't already positioned to reach the loopback interface
//! anyway. Everything else — a public hostname or IP over `http://` — is
//! refused with no override: the whole point is that a user cannot
//! accidentally send a bearer token to `http://some-random-host.example.com`.

use std::net::IpAddr;

/// Refuse `url` unless it is `https://`, or `http://` to a loopback or
/// private-LAN address. Returns the reason as `Err` otherwise.
pub fn ensure_secure_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL '{url}': {e}"))?;

    match parsed.scheme() {
        "https" => Ok(()),
        "http" => {
            let host = parsed
                .host_str()
                .ok_or_else(|| format!("'{url}' has no host"))?;
            if is_loopback_or_private_lan(host) {
                Ok(())
            } else {
                Err(format!(
                    "'{url}' uses plain http:// to a non-local address; use https:// \
                     (only loopback and private-LAN addresses may use http://)"
                ))
            }
        }
        other => Err(format!(
            "'{url}' uses unsupported scheme '{other}'; use https://"
        )),
    }
}

/// Whether `host` is loopback (`localhost`, `127.0.0.0/8`, `::1`) or a
/// private-LAN range (RFC-1918: `10.0.0.0/8`, `172.16.0.0/12` — which
/// includes the docker bridge default `172.17.0.1` — and `192.168.0.0/16`).
fn is_loopback_or_private_lan(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let ip_str = host.trim_start_matches('[').trim_end_matches(']');
    let Ok(ip) = ip_str.parse::<IpAddr>() else {
        return false;
    };
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
        }
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_remote_agent_is_refused() {
        for url in [
            "http://example.com/a2a/agent",
            "http://8.8.8.8/a2a",
            "http://registry.hive.dev",
        ] {
            let err = ensure_secure_url(url).expect_err(url);
            assert!(err.contains("https"), "{err}");
        }
    }

    #[test]
    fn loopback_http_is_allowed() {
        for url in [
            "http://localhost:8080",
            "http://127.0.0.1:11434",
            "http://[::1]:8080",
        ] {
            ensure_secure_url(url).unwrap_or_else(|e| panic!("{url} should pass: {e}"));
        }
    }

    #[test]
    fn hive_registry_requires_https_off_loopback() {
        ensure_secure_url("https://registry.hive.dev").expect("https is always fine");
        ensure_secure_url("http://registry.hive.dev")
            .expect_err("a real, non-local registry must not be reachable over plain http");
    }

    #[test]
    fn private_lan_docker_bridge_is_allowed() {
        // The docker bridge default vLLM/Ollama runs behind (SEC-16's
        // stand-in requirement): must keep working over http://.
        ensure_secure_url("http://172.17.0.1:8000").expect("docker bridge is private-LAN");
        ensure_secure_url("http://10.0.0.5:11434").expect("RFC-1918 10/8 is private-LAN");
        ensure_secure_url("http://192.168.1.20:11434").expect("RFC-1918 192.168/16 is private-LAN");
    }

    #[test]
    fn public_ip_over_http_is_refused() {
        ensure_secure_url("http://93.184.216.34/").expect_err("a public IP is not local");
    }

    #[test]
    fn non_http_schemes_are_refused() {
        ensure_secure_url("ftp://example.com").expect_err("only http(s) are understood");
    }
}
