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
//! // Search for modules (no auth required).
//! let results = client.search("readability").await?;
//! println!("Found {} modules", results.total);
//!
//! // Download a specific version (requires auth token).
//! let auth = client.login("user@example.com", "password123!").await?;
//! let authed = HiveRegistryClient::new("https://registry.example.com")
//!     .with_token(auth.token);
//! let dl = authed.download("readability-auditor", "0.1.0").await?;
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
//! println!("Logged in as {}", auth.username().unwrap_or_default());
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
pub mod credit_guard;
pub mod error;
pub mod models;
pub mod usage;
pub mod verify;

pub use client::HiveRegistryClient;
pub use models::BegunDownload;
pub use credit_guard::{CreditGuard, InsufficientFunds};
pub use error::ClientError;
pub use usage::{UsageCollector, UsageCollectorConfig};
pub use verify::{TrustLevel, VerifyError};
