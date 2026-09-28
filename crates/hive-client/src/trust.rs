//! Which registry root key a client trusts (PL-D3; PL-H5, AGE-608).
//!
//! Every registry download is verified against a root public key the client
//! holds ([`crate::verify`]). The key never comes from the registry, and there
//! is no trust-on-first-use: a client with no root key for its registry
//! refuses every download.
//!
//! - **Production:** [`PRODUCTION_ROOT_PUBLIC_KEY`] is compiled in. It is
//!   empty until the production key is pinned (hive#188), so until then every
//!   non-local registry is refused.
//! - **Local registries** (a loopback host, e.g. the docker compose stack):
//!   [`ROOT_KEY_ENV`] names the root key to trust instead. It is ignored for
//!   any other host, so it can never re-point trust away from the compiled
//!   key for a real registry.

/// The production registry root public key (hex Ed25519), compiled in.
/// `None` until the real key is pinned; the production registry is refused
/// until then.
pub const PRODUCTION_ROOT_PUBLIC_KEY: Option<&str> = None;

/// Names the root public key (hex Ed25519) to trust for a local registry,
/// such as the compose stack's `dev-root-key`.
pub const ROOT_KEY_ENV: &str = "CHATTY_HIVE_ROOT_KEY";

/// The root key to trust for `registry_url`: `override_key` when the
/// registry is local ([`is_local_registry`]) and the override is non-empty,
/// else the compiled production key. `None` means downloads from this
/// registry are refused.
pub fn trusted_root(registry_url: &str, override_key: Option<&str>) -> Option<String> {
    let override_key = override_key.map(str::trim).filter(|k| !k.is_empty());
    match override_key {
        Some(key) if is_local_registry(registry_url) => Some(key.to_ascii_lowercase()),
        _ => PRODUCTION_ROOT_PUBLIC_KEY.map(str::to_string),
    }
}

/// [`trusted_root`] with the override read from [`ROOT_KEY_ENV`].
pub fn trusted_root_from_env(registry_url: &str) -> Option<String> {
    trusted_root(registry_url, std::env::var(ROOT_KEY_ENV).ok().as_deref())
}

/// Whether `registry_url`'s host is loopback (`localhost`, `127.0.0.0/8`,
/// `::1`): a registry on this machine, never the production one.
pub fn is_local_registry(registry_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(registry_url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "43a72e714401762df66b68c26dfbdf2682aaec9f2474eca4613e424a0fbafd3c";

    #[test]
    fn the_override_applies_to_local_registries_only() {
        for local in [
            "http://localhost:8080",
            "http://LOCALHOST:18280/",
            "http://127.0.0.1:41234",
            "http://127.3.2.1",
            "http://[::1]:8080",
        ] {
            assert_eq!(
                trusted_root(local, Some(KEY)).as_deref(),
                Some(KEY),
                "{local}"
            );
        }
        for remote in [
            "https://registry.hive.dev",
            "http://10.0.0.5:8080",
            "http://localhost.evil.example",
            "http://127.0.0.1.nip.io",
            "not a url",
        ] {
            assert_eq!(
                trusted_root(remote, Some(KEY)),
                PRODUCTION_ROOT_PUBLIC_KEY.map(str::to_string),
                "{remote}"
            );
        }
    }

    #[test]
    fn no_key_means_no_trusted_root() {
        // The production slot is empty until the key is pinned (hive#188).
        assert_eq!(PRODUCTION_ROOT_PUBLIC_KEY, None);
        assert_eq!(trusted_root("https://registry.hive.dev", None), None);
        assert_eq!(trusted_root("http://localhost:8080", None), None);
        assert_eq!(trusted_root("http://localhost:8080", Some("  ")), None);
    }
}
