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

Unit tests cover cache eviction and signature verification:

```bash
cargo test -p hive-client
```

[`HiveRegistryClient`]: src/lib.rs
[`models`]: src/models.rs
[`cache`]: src/cache.rs
[`verify`]: src/verify.rs
