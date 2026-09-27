# hive-client

Open-source client library for the Hive module registry.

`HiveRegistryClient` lets you browse, search, download, and verify
Chatty modules from a Hive registry server, with transparent offline
caching and Ed25519 signature verification.

## Public surface

- [`HiveRegistryClient`] — main entry point (`new`, `with_token`,
  `search`, `download`, `login`, …)
- [`models`] — wire types (`ListParams`, `SearchResults`, `DownloadResult`,
  `TrustLevel`, …)
- [`cache`] — on-disk cache used transparently by the client
- [`verify`] — Ed25519 signature verification helpers

See [`src/lib.rs`](src/lib.rs) for the canonical usage example.

## Tests

Unit tests cover cache eviction and signature verification;
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
