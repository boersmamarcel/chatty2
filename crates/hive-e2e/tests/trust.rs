//! S6 · Supply chain and trust (evaluation plan §3 S6, rows 6.1–6.7): the
//! desktop's install flow through the tampering proxy, against a live hive
//! stack. The attacker controls the network or the registry; chatty must
//! refuse what it cannot verify. PL-H5a (AGE-703) fixed rows 6.4, 6.5 and
//! 6.7 (names, download cap, install record re-checked at load); PL-H5
//! (AGE-608) fixes the signature rows 6.1, 6.2 and 6.6, aiming at the
//! boundary PL-D3 (AGE-595) set: a registry root key compiled into chatty
//! signs publisher keys; no TOFU.

use hive_e2e::proxy::{Download, Payload, TamperProxy};
use hive_e2e::{
    SEEDED_VERSION, Stack, User, data_home, install_from_hive, local_module_registry, module_dir,
    unique,
};

/// The seeded module every row attacks, and a second one to swap in.
const TARGET: &str = "echo";
const OTHER: &str = "spin";

/// A signed-in user whose client talks to the registry through a proxy.
async fn through_proxy(stack: &Stack) -> (User, TamperProxy) {
    let user = stack.user().await;
    let proxy = TamperProxy::start(&stack.registry).await;
    (user, proxy)
}

/// Install `name` from the registry as the desktop does, via `proxy`.
async fn install(user: &User, proxy: &TamperProxy, name: &str) -> Result<String, String> {
    let client = user.client(&proxy.url);
    let meta = client
        .get_module(name)
        .await
        .map_err(|e| format!("metadata: {e}"))?;
    install_from_hive(&client, &meta, SEEDED_VERSION)
        .await
        .map(|ext| ext.id)
}

// ── 6.1 ───────────────────────────────────────────────────────────────────

/// 6.1: a download stripped of `x-signature` and `x-publisher-public-key`
/// is refused. Red today: hive-client calls it `TrustLevel::Local` and the
/// installer never reads the trust level (F4) — PL-H5 (AGE-608).
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s6_01_unsigned_download_is_refused() {
    let stack = Stack::from_env();
    let (user, proxy) = through_proxy(&stack).await;
    proxy.set_download(Download::StripSignature);
    let installed = install(&user, &proxy, TARGET).await;
    assert!(
        installed.is_err(),
        "F4 / PL-H5 (AGE-608): {TARGET} installed without a signature or publisher key: {installed:?}"
    );
}

// ── 6.2 ───────────────────────────────────────────────────────────────────

/// 6.2: an attacker's own body, hash, signature and key — self-consistent,
/// but by a key no registry root vouches for — are refused. Red today: the
/// key arrives in the same response and is trusted (F4) — PL-H5 (AGE-608).
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s6_02_attacker_signed_payload_is_refused() {
    let stack = Stack::from_env();
    let (user, proxy) = through_proxy(&stack).await;
    proxy.set_download(Download::Swap(Payload::signed_by_attacker(
        stack.fixture(OTHER),
    )));
    let installed = install(&user, &proxy, TARGET).await;
    assert!(
        installed.is_err(),
        "F4 / PL-H5 (AGE-608): {TARGET} installed with an attacker's body signed by the \
         attacker's own key: {installed:?}"
    );
}

// ── 6.3 ───────────────────────────────────────────────────────────────────

/// 6.3: a tampered body under intact headers is refused (green today: the
/// hash check catches it — keep it that way).
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s6_03_tampered_body_is_refused() {
    let stack = Stack::from_env();
    let (user, proxy) = through_proxy(&stack).await;
    proxy.set_download(Download::CorruptBody);
    let installed = install(&user, &proxy, TARGET).await;
    assert!(
        installed
            .as_ref()
            .is_err_and(|e| e.contains("hash mismatch")),
        "{TARGET} with one byte flipped under genuine headers: {installed:?}"
    );
}

// ── 6.4 ───────────────────────────────────────────────────────────────────

/// 6.4: a registry that names a module `../../<x>` gets no write outside
/// the modules directory. Fixed by PL-H5a (AGE-703): the name is checked
/// against the registry's rule before the download and before any write.
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s6_04_path_traversal_module_name_is_refused_before_any_write() {
    let stack = Stack::from_env();
    let (user, proxy) = through_proxy(&stack).await;
    let escape = unique("pwned");
    let shown = format!("../../{escape}");
    proxy.rename(&shown, TARGET);

    let client = user.client(&proxy.url);
    let meta = client
        .get_module(&shown)
        .await
        .expect("the renamed module's metadata");
    assert_eq!(
        meta.name, shown,
        "the proxy renames the module in the registry's answers"
    );
    let installed = install_from_hive(&client, &meta, SEEDED_VERSION).await;
    let outside = data_home().join(&escape);
    assert!(
        installed.is_err() && !outside.exists(),
        "F5 / PL-H5a (AGE-703): module name {shown:?} was installed ({installed:?}); \
         {} exists: {}",
        outside.display(),
        outside.exists()
    );
}

// ── 6.5 ───────────────────────────────────────────────────────────────────

/// PL-D3's download cap.
const DOWNLOAD_CAP: u64 = 64 << 20;

/// 6.5: a 2 GiB body is aborted at the download cap, not read into memory.
/// The proxy cuts the stream at 256 MiB whatever happens, so the test never
/// holds more. Fixed by PL-H5a (AGE-703): hive-client refuses a declared
/// length over the cap and stops reading at it while streaming.
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s6_05_oversized_download_is_aborted_at_the_cap() {
    let stack = Stack::from_env();
    let (user, proxy) = through_proxy(&stack).await;
    proxy.set_download(Download::Huge {
        declared: 2 << 30,
        serve_at_most: 256 << 20,
    });
    let installed = install(&user, &proxy, TARGET).await;
    let served = proxy.bytes_served();
    // Socket buffers let the proxy run a little ahead of what the client read.
    let slack = 16 << 20;
    assert!(
        installed.is_err() && served <= DOWNLOAD_CAP + slack,
        "PL-H5a (AGE-703): a 2 GiB download was read to {} MiB (cap {} MiB) before it ended: {installed:?}",
        served >> 20,
        DOWNLOAD_CAP >> 20
    );
}

// ── 6.6 ───────────────────────────────────────────────────────────────────

/// 6.6: another module's genuine, signed download replayed under this
/// module's name is refused. Red today: the signature covers only the hash,
/// not name or version — PL-H5 (AGE-608), with PL-H7's signed manifest.
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s6_06_signed_download_replayed_under_another_name_is_refused() {
    let stack = Stack::from_env();
    let (user, proxy) = through_proxy(&stack).await;
    proxy.set_download(Download::Replay {
        name: OTHER.to_string(),
        version: SEEDED_VERSION.to_string(),
    });
    let installed = install(&user, &proxy, TARGET).await;
    assert!(
        installed.is_err(),
        "PL-H5 (AGE-608): {OTHER}@{SEEDED_VERSION}'s signed download was installed as \
         {TARGET}@{SEEDED_VERSION}: {installed:?}"
    );
}

// ── 6.7 ───────────────────────────────────────────────────────────────────

/// 6.7: a module whose `.wasm` changed on disk after install does not load.
/// Fixed by PL-H5a (AGE-703): the install writes `.chatty-install.json`
/// with the hash, and the registry re-checks it at every load.
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s6_07_wasm_tampered_on_disk_after_install_is_refused_at_load() {
    let stack = Stack::from_env();
    let (user, proxy) = through_proxy(&stack).await;
    install(&user, &proxy, TARGET)
        .await
        .expect("a clean install");

    let dir = module_dir().join(TARGET);
    std::fs::write(dir.join(format!("{TARGET}.wasm")), stack.fixture(OTHER)).expect("tamper");
    let loaded = local_module_registry().load(&dir);
    assert!(
        loaded.is_err(),
        "PL-H5a (AGE-703): {} was replaced by {OTHER}'s bytes after install and still loaded as {:?}",
        dir.display(),
        loaded.ok()
    );
}
