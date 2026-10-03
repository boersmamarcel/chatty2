//! Which registry root keys a client trusts (PL-D3; PL-H5, AGE-608;
//! SEC-3, AGE-816).
//!
//! Every registry download is verified against a root public key the client
//! holds ([`crate::verify`]). The key never comes from the registry, and there
//! is no trust-on-first-use: a client with no root key for its registry
//! refuses every download.
//!
//! - **Production:** [`PRODUCTION_ROOT_PUBLIC_KEYS`] is compiled in. A
//!   certificate verifies if *any* listed root signed it, which is how a key
//!   rotation works without a flag day: during a rotation window the list
//!   holds the `current` key and the `next` one.
//!
//!   1. Add `next` to the list and release; both `current` and `next` verify.
//!   2. The registry switches to signing new certificates with `next`.
//!   3. Remove `current` from the list in the following release.
//!
//!   The list is empty until the real key is pinned (SEC-4, hive#188), so
//!   until then every non-local registry is refused.
//! - **Local registries** (a loopback host, e.g. the docker compose stack):
//!   [`ROOT_KEY_ENV`] names the one root key to trust instead. It replaces
//!   the whole compiled list rather than extending it, and it is ignored for
//!   any other host, so it can never re-point trust away from the compiled
//!   keys for a real registry.

/// The production registry root public keys (hex Ed25519), compiled in.
/// Empty until the real key is pinned; the production registry is refused
/// until then. A certificate verifies if any key in the list signed it, so
/// a rotation window holds both the `current` and `next` key (SEC-3,
/// AGE-816).
pub const PRODUCTION_ROOT_PUBLIC_KEYS: &[&str] = &[];

/// Names the root public key (hex Ed25519) to trust for a local registry,
/// such as the compose stack's `dev-root-key`. Set, it replaces the compiled
/// list entirely for that registry; it never adds to it.
pub const ROOT_KEY_ENV: &str = "CHATTY_HIVE_ROOT_KEY";

/// The root keys to trust for `registry_url`: `override_key` alone when the
/// registry is local ([`is_local_registry`]) and the override is non-empty,
/// else the compiled production keys. An empty list means downloads from
/// this registry are refused.
pub fn trusted_roots(registry_url: &str, override_key: Option<&str>) -> Vec<String> {
    select_roots(PRODUCTION_ROOT_PUBLIC_KEYS, registry_url, override_key)
}

/// [`trusted_roots`] with an explicit `production` list, so tests can check
/// the selection rule without depending on what is actually pinned.
fn select_roots(
    production: &[&str],
    registry_url: &str,
    override_key: Option<&str>,
) -> Vec<String> {
    let override_key = override_key.map(str::trim).filter(|k| !k.is_empty());
    match override_key {
        Some(key) if is_local_registry(registry_url) => vec![key.to_ascii_lowercase()],
        _ => production
            .iter()
            .map(|key| key.to_ascii_lowercase())
            .collect(),
    }
}

/// [`trusted_roots`] with the override read from [`ROOT_KEY_ENV`].
pub fn trusted_roots_from_env(registry_url: &str) -> Vec<String> {
    trusted_roots(registry_url, std::env::var(ROOT_KEY_ENV).ok().as_deref())
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

    /// `hive-billing-sdk` (outside the workspace, built for WASM) compiles
    /// its own copy of the root list; a WASM module verifies billing tokens
    /// under it (CX-0b). The two lists must be the same list.
    #[test]
    fn the_billing_sdk_compiles_the_same_root_list() {
        let sdk = include_str!("../../hive-billing-sdk/src/lib.rs");
        let list = |source: &str| -> String {
            let start = source
                .find("pub const PRODUCTION_ROOT_PUBLIC_KEYS: &[&str] = &[")
                .expect("the root list");
            let rest = &source[start..];
            rest[..rest.find("];").expect("the list's end")]
                .split_whitespace()
                .collect()
        };
        assert_eq!(list(sdk), list(include_str!("trust.rs")));
    }

    const KEY: &str = "43a72e714401762df66b68c26dfbdf2682aaec9f2474eca4613e424a0fbafd3c";
    const CURRENT: &str = "11111111111111111111111111111111111111111111111111111111111111";
    const NEXT: &str = "22222222222222222222222222222222222222222222222222222222222222";

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
                trusted_roots(local, Some(KEY)),
                vec![KEY.to_string()],
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
                trusted_roots(remote, Some(KEY)),
                PRODUCTION_ROOT_PUBLIC_KEYS
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>(),
                "{remote}"
            );
        }
    }

    #[test]
    fn no_key_means_no_trusted_root() {
        // The production list is empty until the keys are pinned (hive#188).
        assert!(PRODUCTION_ROOT_PUBLIC_KEYS.is_empty());
        assert_eq!(
            trusted_roots("https://registry.hive.dev", None),
            Vec::<String>::new()
        );
        assert_eq!(
            trusted_roots("http://localhost:8080", None),
            Vec::<String>::new()
        );
        assert_eq!(
            trusted_roots("http://localhost:8080", Some("  ")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn env_override_replaces_the_list() {
        let production: &[&str] = &[CURRENT, NEXT];

        // A local registry with an override trusts the override alone, not
        // the override plus the compiled list.
        assert_eq!(
            select_roots(production, "http://localhost:8080", Some(KEY)),
            vec![KEY.to_string()]
        );

        // A remote (production) registry always gets the full compiled list,
        // regardless of any override.
        assert_eq!(
            select_roots(production, "https://registry.hive.dev", Some(KEY)),
            vec![CURRENT.to_string(), NEXT.to_string()]
        );

        // No override: the local registry falls back to the compiled list too.
        assert_eq!(
            select_roots(production, "http://localhost:8080", None),
            vec![CURRENT.to_string(), NEXT.to_string()]
        );
    }
}
