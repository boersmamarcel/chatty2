//! # hive-client
//!
//! Open-source client library for the Hive module registry.
//!
//! Provides [`HiveRegistryClient`] for browsing, searching and downloading
//! modules from a Hive registry, with transparent offline caching and Ed25519
//! signature verification.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use hive_client::{HiveRegistryClient, models::ListParams};
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let client = HiveRegistryClient::new("https://registry.example.com");
//!
//! // Search for modules.
//! let results = client.search("readability").await?;
//! println!("Found {} modules", results.total);
//!
//! // Download a specific version (requires auth token).
//! let dl = client.download("readability-auditor", "0.1.0").await?;
//! println!("trust: {}", dl.trust_level);
//! # Ok(())
//! # }
//! ```
//!
//! ## Authentication
//!
//! Register and log in to obtain a JWT for download operations:
//!
//! ```rust,no_run
//! use hive_client::HiveRegistryClient;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let client = HiveRegistryClient::new("https://registry.example.com");
//! let auth = client.login("user@example.com", "password123!").await?;
//! println!("Logged in as {}", auth.username);
//! # Ok(())
//! # }
//! ```
//!
//! ## Offline mode
//!
//! Enable the offline cache with [`HiveRegistryClient::with_cache_dir`].
//! Successful list/search responses are persisted as JSON; when the registry
//! is unreachable the cached data is returned so installed modules remain
//! visible.

pub mod cache;
pub mod client;
pub mod error;
pub mod models;
pub mod verify;

pub use client::HiveRegistryClient;
pub use error::ClientError;
pub use verify::{TrustLevel, VerifyError};
