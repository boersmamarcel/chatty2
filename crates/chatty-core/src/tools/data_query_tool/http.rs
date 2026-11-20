//! Reading a data file in place over HTTP: hive's ranged delivery
//! (hive ADR-0089, Files P2-T7, AGE-1652).
//!
//! A hive guest may be given an input not as a copy under `inputs/` but as
//! a lease-authenticated URL on hive's egress file route, which serves HTTP
//! `Range`. DuckDB's `httpfs` extension reads Parquet by range — the footer,
//! then only the row groups and columns a query touches — so one question
//! moves a fraction of the file. The guest agent hands the worker the URL
//! prefix the lease covers and the lease's token ([`HTTP_SCOPE_ENV`],
//! [`HTTP_BEARER_ENV`]), and where the `httpfs` extension is
//! ([`HTTPFS_ENV`]): the bundled DuckDB (`parquet`, `json`) has no `httpfs`
//! built in, and the sandbox keeps extension autoloading off, so it is
//! loaded from a file of DuckDB's exact version.
//!
//! The token becomes a DuckDB `http` secret with that prefix as its
//! `SCOPE`: DuckDB sends `Authorization: Bearer …` only to URLs under it.
//! The token is never in a query, a result or a log; the model's prompt
//! names the URL, not the token.
//!
//! The sandbox is kept: `httpfs` is loaded and the secret created before
//! the connection turns `enable_external_access` off and locks its
//! configuration, and the scope goes into `allowed_directories` beside the
//! workspace. DuckDB checks every path a query opens against that list
//! before any I/O, URLs included, so a URL off the scope (another route on
//! the same host, another host) is a permission error with no request made
//! and no bearer sent, and local reads stay confined to the workspace as
//! before. In a hive guest the microVM reaches nothing but the host's
//! egress proxy in any case, and the lease token opens exactly the
//! resources minted for its lease.
//!
//! Each `query_data` call opens a fresh connection, so `httpfs` (about
//! 20 MB) is loaded per query; DuckDB also fetches the Parquet footer at
//! bind and again at execution. Both are in the measurements of
//! `tests::ranged_parquet_over_http_moves_fewer_bytes_than_the_file`.

use std::path::{Path, PathBuf};

use duckdb::Connection;

use super::DataQueryError;
use super::sql::escape_sql_string;

/// The URL prefix the bearer covers (hive's `…/api/egress/files`).
pub const HTTP_SCOPE_ENV: &str = "CHATTY_DUCKDB_HTTP_SCOPE";
/// The bearer token DuckDB sends to URLs under the scope.
pub const HTTP_BEARER_ENV: &str = "CHATTY_DUCKDB_HTTP_BEARER";
/// The `httpfs` extension file to load; unset, `LOAD httpfs` looks in
/// DuckDB's extension directory (autoinstall stays off).
pub const HTTPFS_ENV: &str = "CHATTY_DUCKDB_HTTPFS";

/// One HTTP scope DuckDB may read in place, with its bearer.
#[derive(Clone)]
pub struct HttpReads {
    scope: String,
    bearer: String,
    httpfs: Option<PathBuf>,
}

impl std::fmt::Debug for HttpReads {
    /// Never the bearer.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpReads")
            .field("scope", &self.scope)
            .field("httpfs", &self.httpfs)
            .finish_non_exhaustive()
    }
}

impl HttpReads {
    /// Reads under `scope` (`http://host/path`, no trailing slash needed)
    /// carry `bearer`.
    pub fn new(scope: impl Into<String>, bearer: impl Into<String>) -> Self {
        Self {
            scope: scope.into().trim_end_matches('/').to_string(),
            bearer: bearer.into(),
            httpfs: None,
        }
    }

    /// Load `httpfs` from this file instead of DuckDB's extension directory.
    pub fn with_httpfs(mut self, path: impl Into<PathBuf>) -> Self {
        self.httpfs = Some(path.into());
        self
    }

    /// The scope and bearer from [`HTTP_SCOPE_ENV`] and [`HTTP_BEARER_ENV`],
    /// the extension from [`HTTPFS_ENV`]; `None` unless both of the first
    /// two are set and non-empty.
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let reads = Self::new(var(HTTP_SCOPE_ENV)?, var(HTTP_BEARER_ENV)?);
        Some(match var(HTTPFS_ENV) {
            Some(path) => reads.with_httpfs(path),
            None => reads,
        })
    }

    /// The URL prefix the bearer covers.
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Whether `url` is under the scope, so a query may read it.
    pub fn covers(&self, url: &str) -> bool {
        url.strip_prefix(&self.scope)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    /// Load `httpfs` and create the bearer secret on `conn`. Runs before
    /// the sandbox locks the configuration, since `LOAD` needs external
    /// access.
    pub(super) fn configure(&self, conn: &Connection) -> Result<(), DataQueryError> {
        let load = match &self.httpfs {
            Some(path) => format!("LOAD '{}'", escape_sql_string(&path.to_string_lossy())),
            None => "LOAD httpfs".to_string(),
        };
        conn.execute_batch(&load).map_err(|e| {
            DataQueryError::QueryFailed(format!(
                "Failed to load DuckDB's httpfs extension for reading files in place: {e}"
            ))
        })?;
        // The secret's name is fixed, so a query cannot read it back by
        // guessing: `duckdb_secrets()` redacts the token in any case.
        conn.execute_batch(&format!(
            "CREATE SECRET hive_files (TYPE http, BEARER_TOKEN '{}', SCOPE '{}');",
            escape_sql_string(&self.bearer),
            escape_sql_string(&self.scope)
        ))
        .map_err(|e| {
            // DuckDB's message could echo the statement; keep it out.
            let _ = e;
            DataQueryError::QueryFailed(
                "Failed to configure the bearer secret for reading files in place".to_string(),
            )
        })?;
        Ok(())
    }

    /// Where the extension is loaded from, if a file.
    pub fn httpfs(&self) -> Option<&Path> {
        self.httpfs.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_covers_urls_under_it_and_the_debug_form_hides_the_bearer() {
        let reads = HttpReads::new("http://172.16.0.1:8081/api/egress/files/", "lease-token");
        assert_eq!(reads.scope(), "http://172.16.0.1:8081/api/egress/files");
        assert!(reads.covers("http://172.16.0.1:8081/api/egress/files/abc"));
        assert!(reads.covers("http://172.16.0.1:8081/api/egress/files"));
        assert!(!reads.covers("http://172.16.0.1:8081/api/egress/filesystem/x"));
        assert!(!reads.covers("http://172.16.0.1:8081/api/egress/seed"));
        assert!(!reads.covers("http://evil/api/egress/files/abc"));
        let debug = format!("{reads:?}");
        assert!(debug.contains("api/egress/files"));
        assert!(!debug.contains("lease-token"), "{debug}");
    }
}
