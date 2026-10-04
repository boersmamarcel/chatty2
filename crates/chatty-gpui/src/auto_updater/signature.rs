//! Release-signature check for the auto-updater (AGE-817, SEC-10).
//!
//! `checksums.txt` proves a download is intact, but it comes from the same
//! GitHub release as the binary, so anyone who can write the release can
//! replace both. `release.yml` therefore signs `checksums.txt` with an Ed25519
//! release key (minisign format) and uploads `checksums.txt.sig`. The updater
//! verifies that signature against the public keys compiled in from
//! `release_keys.txt` **before** it trusts any hash in the file.
//!
//! The trusted list holds several keys so a rotation can overlap (SEC-3's
//! two-key window). An empty list is the one unpinned state: no release key
//! exists yet, so the check is skipped and logged. Once a key is pinned there
//! is no unsigned fallback: a missing or foreign signature refuses the update.

use std::collections::HashMap;

use minisign_verify::{PublicKey, Signature};
use tracing::{info, warn};

/// Name of the signed checksums asset in a release.
pub(super) const CHECKSUMS_ASSET: &str = "checksums.txt";
/// Name of its detached minisign signature.
pub(super) const CHECKSUMS_SIGNATURE_ASSET: &str = "checksums.txt.sig";

/// The release public keys this build trusts, one minisign base64 key per
/// non-comment line.
const RELEASE_KEYS_FILE: &str = include_str!("release_keys.txt");

/// The pinned release public keys (base64 minisign keys).
pub(super) fn pinned_release_keys() -> Vec<&'static str> {
    parse_key_list(RELEASE_KEYS_FILE)
}

fn parse_key_list(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Verify `signature` (the text of `checksums.txt.sig`) over `checksums`
/// against `trusted_keys`, and only then parse the hashes out of it.
///
/// - `trusted_keys` empty: no release key is pinned yet; the check is
///   skipped with a log line and the hashes are returned as before.
/// - signature missing, malformed, or by a key not in the list: `Err`, and
///   the caller refuses the update.
pub(super) fn trusted_checksums(
    checksums: &str,
    signature: Option<&str>,
    trusted_keys: &[&str],
) -> Result<HashMap<String, String>, String> {
    if trusted_keys.is_empty() {
        warn!(
            "No release signing key is pinned in this build; skipping the \
             {CHECKSUMS_SIGNATURE_ASSET} check"
        );
        return Ok(super::AutoUpdater::parse_checksums(checksums));
    }

    let signature = signature.ok_or_else(|| {
        format!(
            "Update refused: the release has no {CHECKSUMS_SIGNATURE_ASSET}, \
             so its checksums cannot be authenticated."
        )
    })?;
    let signature = Signature::decode(signature).map_err(|e| {
        format!("Update refused: {CHECKSUMS_SIGNATURE_ASSET} is not a valid signature ({e}).")
    })?;

    let signed_by_trusted_key = trusted_keys
        .iter()
        .any(|key| match PublicKey::from_base64(key) {
            Ok(public_key) => public_key
                .verify(checksums.as_bytes(), &signature, false)
                .is_ok(),
            Err(e) => {
                warn!(error = %e, "Skipping a malformed pinned release key");
                false
            }
        });
    if !signed_by_trusted_key {
        return Err(format!(
            "Update refused: {CHECKSUMS_SIGNATURE_ASSET} is not a valid signature \
             by a trusted Chatty release key."
        ));
    }

    info!("Release signature on {CHECKSUMS_ASSET} verified");
    Ok(super::AutoUpdater::parse_checksums(checksums))
}

#[cfg(test)]
mod tests {
    use super::*;
    use minisign::KeyPair;
    use std::io::Cursor;

    const ASSET: &str = "chatty-linux-x86_64.AppImage";
    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn checksums() -> String {
        format!("{HASH}  {ASSET}\n")
    }

    fn sign(key: &KeyPair, data: &str) -> String {
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            Cursor::new(data.as_bytes()),
            Some("timestamp:0\tfile:checksums.txt"),
            None,
        )
        .expect("sign fixture checksums")
        .into_string()
    }

    fn keypair() -> KeyPair {
        KeyPair::generate_unencrypted_keypair().expect("generate test key")
    }

    #[test]
    fn unsigned_checksums_refuse_the_update() {
        let key = keypair();
        let pinned = key.pk.to_base64();

        let err = trusted_checksums(&checksums(), None, &[pinned.as_str()])
            .expect_err("a release without checksums.txt.sig must be refused");
        assert!(err.contains("Update refused"), "{err}");
        assert!(err.contains(CHECKSUMS_SIGNATURE_ASSET), "{err}");
    }

    #[test]
    fn a_checksums_signature_from_an_unlisted_key_is_refused() {
        let pinned = keypair();
        let attacker = keypair();
        let pinned_b64 = pinned.pk.to_base64();
        let signature = sign(&attacker, &checksums());

        let err = trusted_checksums(&checksums(), Some(&signature), &[pinned_b64.as_str()])
            .expect_err("a signature by an unlisted key must be refused");
        assert!(err.contains("trusted Chatty release key"), "{err}");
    }

    #[test]
    fn a_valid_signature_lets_the_hash_check_run() {
        // Two pinned keys: the rotation window trusts either.
        let old = keypair();
        let new = keypair();
        let keys = [old.pk.to_base64(), new.pk.to_base64()];
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let signature = sign(&new, &checksums());

        let hashes = trusted_checksums(&checksums(), Some(&signature), &keys)
            .expect("a signature by a pinned key is accepted");
        assert_eq!(hashes.get(ASSET).map(String::as_str), Some(HASH));

        // The signature covers the exact bytes: one swapped hash breaks it.
        let tampered = checksums().replace(&HASH[..4], "ffff");
        assert!(trusted_checksums(&tampered, Some(&signature), &keys).is_err());
    }

    #[test]
    fn an_empty_key_list_skips_the_check() {
        let hashes = trusted_checksums(&checksums(), None, &[]).expect("unpinned build");
        assert_eq!(hashes.get(ASSET).map(String::as_str), Some(HASH));
    }

    #[test]
    fn the_key_list_ignores_comments_and_blank_lines() {
        assert_eq!(
            parse_key_list("# comment\n\n  RWabc  \n# RWnot\nRWdef\n"),
            vec!["RWabc", "RWdef"]
        );
        // Every pinned key in the shipped file must parse.
        for key in pinned_release_keys() {
            PublicKey::from_base64(key).expect("pinned release key is a minisign key");
        }
    }
}
