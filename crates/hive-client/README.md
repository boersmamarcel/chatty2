# hive-client

Open-source client library for the Hive module registry.

`HiveRegistryClient` lets you browse, search, download, and verify
Chatty modules from a Hive registry server, with transparent offline
caching. Every download is verified against a registry root key the client
holds, never one the registry sends; a download that does not verify, or
comes from a registry with no trusted root key, is refused.

## Public surface

- [`HiveRegistryClient`] — main entry point (`new`, `with_token`,
  `search`, `download`, `login`, …)
- [`models`] — wire types (`ListParams`, `SearchResults`, `DownloadResult`,
  `TrustLevel`, …)
- [`cache`] — on-disk cache used transparently by the client
- [`verify`] — the signing chain: root key → publisher certificate →
  signed manifest → the `.wasm`'s SHA-256 (mirrors hive's `hive-verify`)
- [`trust`] — which root key a client trusts: the compiled production key
  (empty until pinned), or `CHATTY_HIVE_ROOT_KEY` for a local (loopback)
  registry such as the compose stack

See [`src/lib.rs`](src/lib.rs) for the canonical usage example.

## Tests

Unit tests cover cache eviction and chain verification;
`tests/vectors.rs` checks the verifier against hive's byte-for-byte vectors
(`tests/vectors/`, copied from hive's `services/hive-verify/tests/vectors/`);
`tests/registry_contract.rs` replays every registry route the client calls
from responses recorded off a real hive-registry (`tests/recorded/`):

```bash
cargo test -p hive-client
```

The recordings are checked against a live registry every night by the
`hive-e2e` suite (`.github/workflows/plugin-e2e.yml`); re-record them with
`HIVE_E2E_RECORD=1` against a running stack (see `scripts/hive-e2e.sh`),
never by hand.

[`HiveRegistryClient`]: src/lib.rs
[`models`]: src/models.rs
[`cache`]: src/cache.rs
[`verify`]: src/verify.rs
[`trust`]: src/trust.rs
