use anyhow::{Context, Result};
use rmcp::service::ServiceExt;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::services::mcp_token_store::FileCredentialStore;
use crate::settings::models::mcp_store::McpServerConfig;

/// Largest OAuth metadata document chatty reads, as rmcp's own OAuth
/// client caps its responses (AGE-1101).
const MAX_OAUTH_METADATA_BYTES: usize = 1024 * 1024;

/// A JSON body of at most [`MAX_OAUTH_METADATA_BYTES`]; `None` when it is
/// larger, cannot be read, or does not parse.
async fn read_json_capped<T: serde::de::DeserializeOwned>(
    mut resp: reqwest::Response,
) -> Option<T> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if body.len() + chunk.len() > MAX_OAUTH_METADATA_BYTES {
            warn!(url = %resp.url(), "OAuth metadata exceeds {MAX_OAUTH_METADATA_BYTES} bytes, refusing it");
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).ok()
}

/// OAuth metadata the server publishes, as rmcp 2's `discover_metadata`
/// found it: rmcp 3's `resolve_metadata` instead synthesizes default
/// endpoints when nothing is published, which is still "no OAuth" here.
async fn discover_metadata(
    auth_manager: &rmcp::transport::auth::AuthorizationManager,
) -> Result<rmcp::transport::auth::AuthorizationMetadata, rmcp::transport::auth::AuthError> {
    use rmcp::transport::auth::{AuthError, AuthorizationMetadataSource};
    let resolution = auth_manager.resolve_metadata().await?;
    if resolution.source == AuthorizationMetadataSource::LegacyEndpointFallback {
        return Err(AuthError::NoAuthorizationSupport);
    }
    Ok(resolution.metadata)
}

/// Whether the configured server is on the LAN, by IP literal or by what
/// its name resolves to; see [`super::ssrf_guard::mcp_server_on_lan`].
async fn server_on_lan(url: &str) -> bool {
    super::ssrf_guard::mcp_server_on_lan(url, &super::ssrf_guard::SystemLookup).await
}

/// rmcp's OAuth manager for the server at `url`. A server on the LAN may
/// name OAuth endpoints on the LAN (`new_allowing_private_addresses`);
/// loopback still needs a loopback server URL and link-local stays refused
/// either way (AGE-1101).
async fn oauth_manager(
    url: &str,
    server_on_lan: bool,
) -> Result<rmcp::transport::auth::AuthorizationManager, rmcp::transport::auth::AuthError> {
    use rmcp::transport::auth::AuthorizationManager;
    if server_on_lan {
        AuthorizationManager::new_allowing_private_addresses(url).await
    } else {
        AuthorizationManager::new(url).await
    }
}

/// Represents an active MCP server connection
pub struct McpConnection {
    /// Server name
    pub name: String,

    /// The rmcp service for communicating with the server
    pub service: rmcp::service::RunningService<rmcp::RoleClient, ()>,

    /// Cached tool list, populated on first fetch and invalidated on reconnect
    cached_tools: Option<Vec<rmcp::model::Tool>>,
}

impl McpConnection {
    /// Connect to an already-running MCP server via its HTTP endpoint.
    ///
    /// If the server requires OAuth authentication, an interactive browser-based
    /// flow is initiated automatically.
    pub async fn connect(config: McpServerConfig) -> Result<Self> {
        hive_client::ensure_secure_url(&config.url).map_err(anyhow::Error::msg)?;

        let name = config.name.clone();
        let url = config.url.clone();

        info!(
            server = %name,
            url = %url,
            has_api_key = config.has_api_key(),
            "Connecting to MCP server"
        );

        // Probe: check if the server advertises OAuth via the MCP resource metadata
        // endpoint. This catches servers that return 401 without WWW-Authenticate
        // headers (e.g. Homey), which rmcp can't auto-detect as AuthRequired.
        // Also catches servers where GET returns 200 HTML (e.g. HuggingFace),
        // which causes rmcp's discover_metadata() to fail on JSON parsing.
        if config.api_key.as_ref().is_none_or(|k| k.is_empty())
            && let Some(auth_servers) = Self::probe_oauth_metadata(&url).await
        {
            info!(
                server = %name,
                "Server advertises OAuth via resource metadata, using OAuth flow"
            );
            return Self::connect_with_oauth(&name, &url, Some(auth_servers)).await;
        }

        // Try connecting with retries for transient transport errors (e.g.
        // local gateway not yet listening at startup).
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

        let mut transport_config = StreamableHttpClientTransportConfig::with_uri(url.as_str());
        if let Some(key) = config.api_key.filter(|k| !k.is_empty()) {
            transport_config = transport_config.auth_header(key);
        }

        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                info!(
                    server = %name,
                    attempt,
                    "Retrying MCP connection after transport error"
                );
                tokio::time::sleep(RETRY_DELAY).await;
            }

            let transport = StreamableHttpClientTransport::from_config(transport_config.clone());

            match ().serve(transport).await {
                Ok(service) => {
                    let server_info = service.peer_info();
                    info!(
                        server = %name,
                        info = ?server_info,
                        "MCP server connected"
                    );
                    return Ok(Self {
                        name,
                        service,
                        cached_tools: None,
                    });
                }
                Err(e) => {
                    let err_str = format!("{e:?}");
                    if err_str.contains("Auth required")
                        || err_str.contains("AuthRequired")
                        || err_str.contains("error decoding response body")
                    {
                        info!(
                            server = %name,
                            "Server requires OAuth, starting browser authorization flow"
                        );
                        last_err = None;
                        break;
                    }

                    // Retry on transport/connection errors
                    let is_transport_error = err_str.contains("error sending request")
                        || err_str.contains("connection refused")
                        || err_str.contains("Connection refused")
                        || err_str.contains("Client error");

                    if is_transport_error && attempt < MAX_RETRIES {
                        warn!(
                            server = %name,
                            attempt,
                            "Transport error, will retry: {e}"
                        );
                        last_err = Some(e.into());
                        continue;
                    }

                    return Err(e)
                        .with_context(|| format!("Failed to connect to MCP server: {}", name));
                }
            }
        }

        if let Some(e) = last_err {
            return Err(e).with_context(|| format!("Failed to connect to MCP server: {}", name));
        }

        // Second attempt: OAuth 2.0 browser-based authorization
        Self::connect_with_oauth(&name, &url, None).await
    }

    /// Probe whether the MCP server advertises OAuth-protected resource metadata.
    ///
    /// Returns `Some(authorization_servers)` if the server has a
    /// `.well-known/oauth-protected-resource` endpoint, `None` otherwise.
    ///
    /// The probe goes through the same guarded client as OAuth discovery
    /// (AGE-1101): no redirects, and the address it connects to must pass
    /// [`mcp_oauth_endpoint`](super::ssrf_guard::mcp_oauth_endpoint), so a
    /// public server cannot redirect it into the LAN or to loopback.
    async fn probe_oauth_metadata(url: &str) -> Option<Vec<String>> {
        let server_on_lan = server_on_lan(url).await;
        let client = super::http_client::mcp_oauth_client(5, url, server_on_lan);
        Self::probe_oauth_metadata_with(&client, url, server_on_lan).await
    }

    async fn probe_oauth_metadata_with(
        client: &reqwest::Client,
        url: &str,
        server_on_lan: bool,
    ) -> Option<Vec<String>> {
        let base = url.trim_end_matches('/');
        let origin = if let Ok(parsed) = reqwest::Url::parse(base) {
            format!("{}://{}", parsed.scheme(), parsed.authority())
        } else {
            return None;
        };

        let metadata_url = format!("{origin}/.well-known/oauth-protected-resource");
        super::ssrf_guard::check_mcp_oauth_url_without_lookup(url, server_on_lan, &metadata_url)
            .ok()?;

        let resp = client.get(&metadata_url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }

        #[derive(serde::Deserialize)]
        struct ResourceMetadata {
            authorization_servers: Option<Vec<String>>,
        }

        let meta: ResourceMetadata = read_json_capped(resp).await?;
        let servers = meta.authorization_servers.filter(|s| !s.is_empty());
        if servers.is_some() {
            debug!(url = %metadata_url, "Found OAuth resource metadata");
        }
        servers
    }

    /// Manually discover OAuth authorization server metadata by fetching the
    /// `.well-known/oauth-authorization-server` endpoint from each candidate.
    ///
    /// This bypasses rmcp's `discover_metadata()` which fails when the MCP
    /// endpoint returns 200 HTML for GET requests (e.g. HuggingFace serves an
    /// HTML page at `/mcp`), causing rmcp to treat it as resource metadata
    /// and fail on JSON parsing.
    ///
    /// The server names those hosts, so they are fetched through
    /// [`mcp_oauth_client`](super::http_client::mcp_oauth_client): no
    /// loopback, private or link-local address unless `server_url` itself is
    /// local (or, for private-LAN addresses, on the LAN), no redirects, and a
    /// capped body (AGE-1101).
    async fn discover_auth_metadata_manually(
        server_url: &str,
        server_on_lan: bool,
        auth_servers: &[String],
    ) -> Option<rmcp::transport::auth::AuthorizationMetadata> {
        let client = super::http_client::mcp_oauth_client(10, server_url, server_on_lan);
        Self::discover_auth_metadata_with(&client, server_url, server_on_lan, auth_servers).await
    }

    async fn discover_auth_metadata_with(
        client: &reqwest::Client,
        server_url: &str,
        server_on_lan: bool,
        auth_servers: &[String],
    ) -> Option<rmcp::transport::auth::AuthorizationMetadata> {
        let get = |url: String| async move {
            if let Err(e) = super::ssrf_guard::check_mcp_oauth_url_without_lookup(
                server_url,
                server_on_lan,
                &url,
            ) {
                warn!(url = %url, error = %e, "Refusing OAuth metadata URL");
                return None;
            }
            match client.get(&url).send().await {
                Ok(r) if r.status().is_success() => Some(r),
                Ok(_) => None,
                Err(e) => {
                    debug!(url = %url, error = %e, "OAuth metadata request failed");
                    None
                }
            }
        };

        for server in auth_servers {
            let server = server.trim_end_matches('/');
            let metadata_url = format!("{server}/.well-known/oauth-authorization-server");

            debug!(url = %metadata_url, "Trying manual auth metadata discovery");

            let resp = match get(metadata_url).await {
                Some(r) => r,
                // Also try openid-configuration as fallback
                None => match get(format!("{server}/.well-known/openid-configuration")).await {
                    Some(r) => r,
                    None => continue,
                },
            };

            match read_json_capped::<rmcp::transport::auth::AuthorizationMetadata>(resp).await {
                Some(metadata) => {
                    debug!(server = %server, "Found authorization metadata");
                    return Some(metadata);
                }
                None => {
                    warn!(server = %server, "Failed to read authorization metadata");
                }
            }
        }

        None
    }

    /// Perform the full OAuth 2.0 authorization code flow with PKCE:
    /// 1. Discover OAuth metadata from the server
    /// 2. Dynamically register the client
    /// 3. Start a local HTTP callback server
    /// 4. Open the user's browser for authorization
    /// 5. Exchange the authorization code for an access token
    /// 6. Connect using the authorized client
    ///
    /// Tokens are persisted via `FileCredentialStore` so subsequent connections
    /// can skip the browser flow.
    ///
    /// `probed_auth_servers`: if our probe already found the authorization servers
    /// via `.well-known/oauth-protected-resource`, pass them here to bypass
    /// rmcp's `discover_metadata()` which fails on some servers (e.g. HuggingFace
    /// returns 200 HTML for GET to the MCP endpoint, confusing rmcp's discovery).
    async fn connect_with_oauth(
        name: &str,
        url: &str,
        probed_auth_servers: Option<Vec<String>>,
    ) -> Result<Self> {
        use rmcp::transport::auth::{AuthClient, CredentialStore};

        let credential_store = FileCredentialStore::for_server(name);

        // Fast path: try connecting with cached tokens first
        if let Some(creds) = credential_store.load().await.unwrap_or(None)
            && creds.token_response.is_some()
        {
            info!(server = %name, "Found cached OAuth tokens, attempting reconnection");
            match Self::connect_with_cached_oauth(
                name,
                url,
                credential_store.clone(),
                probed_auth_servers.clone(),
            )
            .await
            {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    warn!(
                        server = %name,
                        error = ?e,
                        "Cached OAuth tokens failed, falling back to browser flow"
                    );
                    // Clear stale tokens
                    credential_store.clear().await.ok();
                }
            }
        }

        // Full browser-based OAuth flow
        let server_on_lan = server_on_lan(url).await;
        let mut auth_manager = oauth_manager(url, server_on_lan)
            .await
            .with_context(|| format!("Failed to create OAuth manager for {name}"))?;

        // rmcp's own OAuth client: discovery does not follow redirects (some
        // servers, e.g. Homey, redirect their root URL to a marketing page), it
        // refuses loopback, private and link-local addresses unless the server
        // URL is itself local, and it caps response bodies. `with_client` would
        // drop the address check (RUSTSEC-2026-0457, AGE-1101).

        // Set file-backed credential store so tokens are auto-persisted
        auth_manager.set_credential_store(credential_store);

        // Discover authorization server metadata.
        // When our probe already identified the auth servers, do manual discovery
        // to bypass rmcp's discover_metadata() which fails when the MCP endpoint
        // returns 200 HTML for GET requests (e.g. HuggingFace).
        let metadata = if let Some(ref servers) = probed_auth_servers {
            if let Some(meta) =
                Self::discover_auth_metadata_manually(url, server_on_lan, servers).await
            {
                meta
            } else {
                // Manual discovery failed; fall back to rmcp's built-in discovery
                warn!(server = %name, "Manual metadata discovery failed, trying rmcp discovery");
                discover_metadata(&auth_manager)
                    .await
                    .with_context(|| format!("Failed to discover OAuth metadata for {name}"))?
            }
        } else {
            discover_metadata(&auth_manager)
                .await
                .with_context(|| format!("Failed to discover OAuth metadata for {name}"))?
        };

        // Check if the server requires form_post response mode (e.g. Homey)
        let needs_form_post = metadata
            .additional_fields
            .get("response_modes_supported")
            .and_then(|v| v.as_array())
            .map(|modes| {
                modes.iter().any(|m| m.as_str() == Some("form_post"))
                    && !modes.iter().any(|m| m.as_str() == Some("query"))
            })
            .unwrap_or(false);

        auth_manager.set_metadata(metadata);

        // Pick a random port for the local callback server
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind local OAuth callback server")?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://localhost:{port}/callback");

        info!(server = %name, redirect_uri = %redirect_uri, "Starting OAuth callback server");

        // Register the client dynamically and get the authorization URL
        let scopes = auth_manager.select_scopes(None, &[]);
        let scope_refs: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();

        let oauth_config = match auth_manager
            .register_client("Chatty", &redirect_uri, &scope_refs)
            .await
        {
            Ok(config) => config,
            Err(e) => {
                let err_str = format!("{e:?}");
                if err_str.contains("403") || err_str.contains("Forbidden") {
                    return Err(anyhow::anyhow!(
                        "OAuth client registration was rejected by {name}. \
                         This server only allows pre-approved MCP clients. \
                         Try using the server's local/desktop MCP option instead."
                    ));
                }
                if err_str.contains("Dynamic client registration not supported") {
                    return Err(anyhow::anyhow!(
                        "{name} does not support browser sign-in (OAuth dynamic client \
                         registration). Add an access token as this server's API key in \
                         Settings → Extensions, then enable it again."
                    ));
                }
                return Err(anyhow::anyhow!(e))
                    .with_context(|| format!("Failed to register OAuth client for {name}"));
            }
        };
        auth_manager.configure_client(oauth_config)?;

        let mut auth_url = auth_manager
            .get_authorization_url(&scope_refs)
            .await
            .with_context(|| format!("Failed to get OAuth authorization URL for {name}"))?;

        // Append response_mode=form_post when the server requires it
        if needs_form_post {
            let separator = if auth_url.contains('?') { "&" } else { "?" };
            auth_url = format!("{auth_url}{separator}response_mode=form_post");
            info!(server = %name, "Using form_post response mode for OAuth callback");
        }

        // Open the browser
        info!(server = %name, "Opening browser for OAuth authorization");
        if let Err(e) = Self::open_browser(&auth_url) {
            warn!(
                server = %name,
                error = ?e,
                url = %auth_url,
                "Failed to open browser — please open this URL manually"
            );
        }

        // Wait for the OAuth callback
        let (code, state, issuer) = Self::wait_for_oauth_callback(listener).await?;

        // Exchange the authorization code for an access token
        // (FileCredentialStore automatically persists the token via CredentialStore::save)
        info!(server = %name, "Exchanging OAuth code for token");
        auth_manager
            .exchange_code_for_token_with_issuer(&code, &state, issuer.as_deref())
            .await
            .with_context(|| format!("Failed to exchange OAuth code for {name}"))?;

        // Build an authorized transport and connect
        let auth_client = AuthClient::new(super::http_client::default_client(30), auth_manager);

        let transport = StreamableHttpClientTransport::with_client(
            auth_client,
            StreamableHttpClientTransportConfig::with_uri(url),
        );

        let service = ()
            .serve(transport)
            .await
            .with_context(|| format!("Failed to connect to MCP server after OAuth: {name}"))?;

        let server_info = service.peer_info();
        info!(
            server = %name,
            info = ?server_info,
            "MCP server connected via OAuth"
        );

        Ok(Self {
            name: name.to_string(),
            service,
            cached_tools: None,
        })
    }

    /// Connect using previously cached OAuth credentials.
    ///
    /// Creates an `AuthorizationManager` with the file-backed credential store,
    /// discovers metadata, and lets `AuthClient` handle token injection/refresh.
    async fn connect_with_cached_oauth(
        name: &str,
        url: &str,
        credential_store: FileCredentialStore,
        probed_auth_servers: Option<Vec<String>>,
    ) -> Result<Self> {
        use rmcp::transport::auth::AuthClient;

        let server_on_lan = server_on_lan(url).await;
        let mut auth_manager = oauth_manager(url, server_on_lan)
            .await
            .with_context(|| format!("Failed to create OAuth manager for {name}"))?;

        // rmcp's own OAuth client (same rationale as connect_with_oauth)
        auth_manager.set_credential_store(credential_store);

        // Use manual discovery when probed_auth_servers is available (same as connect_with_oauth)
        let metadata = if let Some(ref servers) = probed_auth_servers {
            if let Some(meta) =
                Self::discover_auth_metadata_manually(url, server_on_lan, servers).await
            {
                meta
            } else {
                discover_metadata(&auth_manager)
                    .await
                    .with_context(|| format!("Failed to discover OAuth metadata for {name}"))?
            }
        } else {
            discover_metadata(&auth_manager)
                .await
                .with_context(|| format!("Failed to discover OAuth metadata for {name}"))?
        };
        auth_manager.set_metadata(metadata);

        let auth_client = AuthClient::new(super::http_client::default_client(30), auth_manager);

        let transport = StreamableHttpClientTransport::with_client(
            auth_client,
            StreamableHttpClientTransportConfig::with_uri(url),
        );

        let service = ()
            .serve(transport)
            .await
            .with_context(|| format!("Failed to connect with cached OAuth tokens: {name}"))?;

        let server_info = service.peer_info();
        info!(
            server = %name,
            info = ?server_info,
            "MCP server connected via cached OAuth tokens"
        );

        Ok(Self {
            name: name.to_string(),
            service,
            cached_tools: None,
        })
    }

    /// Run a minimal HTTP server that accepts exactly one callback request,
    /// extracts `code`, `state` and the optional RFC 9207 `iss` parameters, sends a success page, and returns.
    ///
    /// Supports both `GET /callback?code=X&state=Y` (standard query mode)
    /// and `POST /callback` with URL-encoded form body (`response_mode=form_post`,
    /// used by servers like Homey).
    async fn wait_for_oauth_callback(
        listener: tokio::net::TcpListener,
    ) -> Result<(String, String, Option<String>)> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        info!("Waiting for OAuth callback…");

        let (mut stream, _addr) =
            tokio::time::timeout(std::time::Duration::from_secs(300), listener.accept())
                .await
                .context("Timed out waiting for OAuth callback (5 min)")?
                .context("Failed to accept OAuth callback connection")?;

        // Read the HTTP request (headers + body)
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse the request line
        let request_line = request.lines().next().unwrap_or("");
        let method = request_line.split_whitespace().next().unwrap_or("GET");
        let path = request_line.split_whitespace().nth(1).unwrap_or("/");

        // Extract code and state from either GET query params or POST form body
        let params: HashMap<String, String> = if method.eq_ignore_ascii_case("POST") {
            // POST form_post: params are in the URL-encoded body after \r\n\r\n
            let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
            Self::parse_query_string(body)
        } else {
            // GET: params are in the query string
            let query_string = path.split('?').nth(1).unwrap_or("");
            Self::parse_query_string(query_string)
        };

        let code = params
            .get("code")
            .cloned()
            .context("OAuth callback missing 'code' parameter")?;

        let state = params
            .get("state")
            .cloned()
            .context("OAuth callback missing 'state' parameter")?;

        // RFC 9207 `iss`: servers such as Linear advertise it as required, and
        // the token exchange rejects the response if we drop it.
        let issuer = params.get("iss").cloned();

        // Send a success response
        let body = "<!DOCTYPE html><html><body>\
            <h2>✓ Authorization successful</h2>\
            <p>You can close this tab and return to Chatty.</p>\
            </body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/html\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.ok();
        stream.shutdown().await.ok();

        info!(method = %method, "OAuth callback received successfully");
        Ok((code, state, issuer))
    }

    /// Parse a URL-encoded query string into key-value pairs.
    fn parse_query_string(input: &str) -> HashMap<String, String> {
        input
            .split('&')
            .filter(|s| !s.is_empty())
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                let key = parts.next()?;
                let value = parts.next().unwrap_or("");
                Some((key.to_string(), Self::percent_decode(value)))
            })
            .collect()
    }

    /// Simple percent-decoding for OAuth callback query params.
    fn percent_decode(input: &str) -> String {
        let mut result = String::with_capacity(input.len());
        let mut chars = input.bytes();
        while let Some(b) = chars.next() {
            if b == b'%' {
                let hi = chars.next().unwrap_or(b'0');
                let lo = chars.next().unwrap_or(b'0');
                let hex = [hi, lo];
                if let Ok(s) = std::str::from_utf8(&hex)
                    && let Ok(val) = u8::from_str_radix(s, 16)
                {
                    result.push(val as char);
                    continue;
                }
                result.push('%');
                result.push(hi as char);
                result.push(lo as char);
            } else if b == b'+' {
                result.push(' ');
            } else {
                result.push(b as char);
            }
        }
        result
    }

    /// Open a URL in the user's default browser.
    fn open_browser(url: &str) -> Result<()> {
        let platform = if cfg!(target_os = "windows") {
            BrowserPlatform::Windows
        } else if cfg!(target_os = "macos") {
            BrowserPlatform::MacOs
        } else {
            BrowserPlatform::Other
        };
        let (program, args) = browser_launch_argv(platform, url)?;
        std::process::Command::new(program).args(args).spawn()?;
        Ok(())
    }

    /// Gracefully disconnect from the server
    pub async fn disconnect(self) -> Result<()> {
        info!(server = %self.name, "Disconnecting from MCP server");

        self.service
            .cancel()
            .await
            .with_context(|| format!("Failed to cancel MCP service: {}", self.name))?;

        Ok(())
    }
}

/// Global service for managing MCP server connections
#[derive(Clone)]
pub struct McpService {
    /// Active connections keyed by server name.
    ///
    /// A `BTreeMap` so every enumeration (tool gathering, disconnect) is in
    /// server-name order. With a `HashMap` the tool-definition block sent to
    /// the model was ordered by the process's random hasher seed, so it
    /// differed across restarts and invalidated the provider's prompt cache
    /// on every launch (AGE-206).
    connections: Arc<RwLock<BTreeMap<String, McpConnection>>>,
}

impl McpService {
    /// Create a new MCP service
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Connect to a single MCP server by URL.
    pub async fn connect_server(&self, config: McpServerConfig) -> Result<()> {
        let name = config.name.clone();

        // Check if already connected
        {
            let connections = self.connections.read().await;
            if connections.contains_key(&name) {
                warn!(server = %name, "MCP server already connected");
                return Ok(());
            }
        }

        let connection = McpConnection::connect(config).await?;

        {
            let mut connections = self.connections.write().await;
            connections.insert(name.clone(), connection);
        }

        info!(server = %name, "MCP server connected successfully");
        Ok(())
    }

    /// Disconnect from a single MCP server.
    pub async fn disconnect_server(&self, name: &str) -> Result<()> {
        let connection = {
            let mut connections = self.connections.write().await;
            connections.remove(name)
        };

        if let Some(connection) = connection {
            connection.disconnect().await?;
            info!(server = %name, "MCP server disconnected");
        } else {
            warn!(server = %name, "MCP server not found");
        }

        Ok(())
    }

    /// Connect to all enabled servers from the given configurations concurrently.
    pub async fn connect_all(&self, configs: Vec<McpServerConfig>) -> Result<()> {
        let _ = self.connect_all_with_status(configs).await;
        Ok(())
    }

    /// Connect to all enabled servers, returning per-server results.
    /// Returns a list of (server_name, success: bool, error_message: Option).
    pub async fn connect_all_with_status(
        &self,
        configs: Vec<McpServerConfig>,
    ) -> Vec<(String, bool, Option<String>)> {
        info!(count = configs.len(), "Connecting to MCP servers");

        let mut join_set = tokio::task::JoinSet::new();

        for config in configs {
            if !config.enabled {
                debug!(server = %config.name, "Skipping disabled MCP server");
                continue;
            }

            let svc = self.clone();
            join_set.spawn(async move {
                let name = config.name.clone();
                match svc.connect_server(config).await {
                    Ok(()) => (name, true, None),
                    Err(e) => {
                        let err_msg = format!("{e:#}");
                        error!(server = %name, error = ?e, "Failed to connect to MCP server");
                        (name, false, Some(err_msg))
                    }
                }
            });
        }

        let mut results = Vec::new();
        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok(result) => results.push(result),
                Err(e) => warn!(error = ?e, "MCP server connect task panicked"),
            }
        }

        let error_count = results.iter().filter(|(_, ok, _)| !ok).count();
        if error_count > 0 {
            warn!(failed = error_count, "Some MCP servers failed to connect");
        }

        results
    }

    /// Disconnect from all connected servers
    pub async fn disconnect_all(&self) -> Result<()> {
        let server_names: Vec<String> = {
            let connections = self.connections.read().await;
            connections.keys().cloned().collect()
        };

        info!(
            count = server_names.len(),
            "Disconnecting from all MCP servers"
        );

        for name in server_names {
            if let Err(e) = self.disconnect_server(&name).await {
                error!(
                    server = %name,
                    error = ?e,
                    "Failed to disconnect from MCP server"
                );
            }
        }

        Ok(())
    }

    /// Get all tools from all active servers, grouped by server with their ServerSinks.
    ///
    /// Tool lists are cached after the first successful fetch per server.
    ///
    /// Takes only a read lock to snapshot each connection's peer and cached
    /// state, in `BTreeMap` order (AGE-206); a cold cache is listed via the
    /// cloned peer outside any lock and the result is written back with a
    /// brief write lock. No `.await` runs while a write guard is held
    /// (AGE-228) — previously a cold cache serialised every concurrent
    /// conversation open behind the network round trip.
    pub async fn get_all_tools_with_sinks(
        &self,
    ) -> Result<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>> {
        let snapshot: Vec<(
            String,
            rmcp::service::ServerSink,
            Option<Vec<rmcp::model::Tool>>,
        )> = {
            let connections = self.connections.read().await;
            connections
                .iter()
                .map(|(name, connection)| {
                    (
                        name.clone(),
                        connection.service.peer().clone(),
                        connection.cached_tools.clone(),
                    )
                })
                .collect()
        };

        let mut result = Vec::with_capacity(snapshot.len());

        for (name, server_sink, cached) in snapshot {
            let tools = match cached {
                Some(tools) => {
                    debug!(server = %name, "Returning cached tool list");
                    tools
                }
                None => match server_sink.list_tools(Default::default()).await {
                    Ok(response) => {
                        let mut connections = self.connections.write().await;
                        if let Some(connection) = connections.get_mut(&name) {
                            connection.cached_tools = Some(response.tools.clone());
                        }
                        response.tools
                    }
                    Err(e) => {
                        error!(
                            server = %name,
                            error = ?e,
                            "Failed to list tools from MCP server"
                        );
                        continue;
                    }
                },
            };

            let tool_count = tools.len();
            for tool in &tools {
                debug!(
                    server = %name,
                    tool_name = %tool.name,
                    "Retrieved tool from MCP server"
                );
            }

            result.push((name.clone(), tools, server_sink));
            info!(
                server = %name,
                tool_count = tool_count,
                "Retrieved tools with ServerSink"
            );
        }

        Ok(result)
    }
}

impl Default for McpService {
    fn default() -> Self {
        Self::new()
    }
}

/// Platform families that launch the default browser differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserPlatform {
    Windows,
    MacOs,
    Other,
}

/// Build the program and argv that open `url` in the default browser.
///
/// The URL is always a single argument and never passes through a shell:
/// on Windows `cmd /C start` would re-parse `&` (the authorization URL is
/// partly server-controlled, so that was command injection), hence
/// `rundll32 url.dll,FileProtocolHandler`. Only http(s) URLs are accepted.
fn browser_launch_argv(
    platform: BrowserPlatform,
    url: &str,
) -> Result<(&'static str, Vec<String>)> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        anyhow::bail!("refusing to open a non-http(s) URL in the browser");
    }
    Ok(match platform {
        BrowserPlatform::Windows => (
            "rundll32",
            vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()],
        ),
        BrowserPlatform::MacOs => ("open", vec![url.to_string()]),
        BrowserPlatform::Other => ("xdg-open", vec![url.to_string()]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::mcp_store::McpServerConfig;

    fn disabled_config(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            url: "http://localhost:3000/mcp".to_string(),
            api_key: None,
            enabled: false,
            is_module: false,
        }
    }

    // --- McpService::new / Default ---

    #[test]
    fn test_new_service_has_no_connections() {
        let svc = McpService::new();
        // A freshly created service should have no active connections
        // (verified by get_all_tools_with_sinks returning empty in async tests)
        let _ = svc.connections.try_read().is_ok();
    }

    #[test]
    fn test_default_equals_new() {
        let _svc = McpService::default();
        // Default constructor delegates to new() — just verify it doesn't panic
    }

    // --- disconnect_server on unknown name ---

    #[tokio::test]
    async fn test_disconnect_server_unknown_is_ok() {
        let svc = McpService::new();
        let result = svc.disconnect_server("nonexistent").await;
        assert!(result.is_ok());
    }

    // --- connect_all skips disabled servers ---

    #[tokio::test]
    async fn test_connect_all_skips_disabled_servers() {
        let svc = McpService::new();
        let configs = vec![disabled_config("disabled-a"), disabled_config("disabled-b")];
        let result = svc.connect_all(configs).await;
        assert!(result.is_ok());

        // No connections should have been registered for disabled servers
        let connections = svc.connections.read().await;
        assert!(connections.is_empty());
    }

    #[tokio::test]
    async fn test_connect_all_empty_list_is_ok() {
        let svc = McpService::new();
        let result = svc.connect_all(vec![]).await;
        assert!(result.is_ok());
    }

    // --- get_all_tools_with_sinks with no connections ---

    #[tokio::test]
    async fn test_get_all_tools_no_connections_returns_empty() {
        let svc = McpService::new();
        let result = svc.get_all_tools_with_sinks().await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    // --- disconnect_all with no connections ---

    #[tokio::test]
    async fn test_disconnect_all_empty_is_ok() {
        let svc = McpService::new();
        let result = svc.disconnect_all().await;
        assert!(result.is_ok());
    }

    // --- connect_all: bad URL returns Ok (non-fatal, errors are logged) ---

    #[tokio::test]
    async fn test_connect_all_bad_url_returns_ok() {
        let svc = McpService::new();
        let configs = vec![
            McpServerConfig {
                name: "bad-1".to_string(),
                url: "http://127.0.0.1:1/mcp".to_string(),
                api_key: None,
                enabled: true,
                is_module: false,
            },
            McpServerConfig {
                name: "bad-2".to_string(),
                url: "http://127.0.0.1:2/mcp".to_string(),
                api_key: None,
                enabled: true,
                is_module: false,
            },
        ];

        let result = svc.connect_all(configs).await;
        // connect_all returns Ok even when all servers fail to connect
        assert!(result.is_ok());
    }

    /// SEC-16 / AGE-756: an MCP server configured over plain `http://` to a
    /// real host is refused before any request is dialed — its bearer token
    /// never has the chance to cross the wire in clear text.
    #[tokio::test]
    async fn mcp_server_over_plain_http_is_refused() {
        let config = McpServerConfig {
            name: "remote".to_string(),
            url: "http://mcp.example.com/mcp".to_string(),
            api_key: None,
            enabled: true,
            is_module: false,
        };
        let result = McpConnection::connect(config).await;
        match result {
            Ok(_) => panic!("a plain-http remote MCP server must be refused"),
            Err(err) => assert!(format!("{err:#}").contains("https"), "{err:#}"),
        }
    }

    // --- Tool cache: get_all_tools idempotent with no connections ---

    #[tokio::test]
    async fn test_get_all_tools_idempotent_no_connections() {
        let svc = McpService::new();
        let r1 = svc.get_all_tools_with_sinks().await.unwrap();
        let r2 = svc.get_all_tools_with_sinks().await.unwrap();
        assert_eq!(r1.len(), r2.len());
    }

    #[test]
    fn browser_argv_keeps_full_url_as_one_argument() {
        let url = "https://x/auth?client_id=a&state=b&calc.exe&";
        let (prog, args) = browser_launch_argv(BrowserPlatform::Windows, url).unwrap();
        assert_eq!(prog, "rundll32");
        assert_eq!(args, vec!["url.dll,FileProtocolHandler", url]);
        assert_eq!(
            browser_launch_argv(BrowserPlatform::MacOs, url).unwrap(),
            ("open", vec![url.to_string()])
        );
        assert_eq!(
            browser_launch_argv(BrowserPlatform::Other, url).unwrap(),
            ("xdg-open", vec![url.to_string()])
        );
    }

    #[test]
    fn browser_argv_refuses_non_http_schemes() {
        for url in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "calc.exe",
            "ms-msdt:/id",
            "",
        ] {
            assert!(browser_launch_argv(BrowserPlatform::Windows, url).is_err());
        }
        assert!(browser_launch_argv(BrowserPlatform::Windows, "HTTP://x/?a=1").is_ok());
    }

    /// A one-route HTTP server on loopback answering every request with an
    /// OAuth metadata document; the counter is the requests it received.
    async fn metadata_server() -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                let body = r#"{"authorization_endpoint":"https://auth.example.com/authorize","token_endpoint":"https://auth.example.com/token"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (port, hits)
    }

    /// Resolves every name to loopback: a public-looking authorization
    /// server name that the MCP server's DNS points at this machine.
    struct ResolvesToLoopback;

    impl crate::services::ssrf_guard::HostLookup for ResolvesToLoopback {
        fn lookup(
            &self,
            _host: &str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<Vec<std::net::IpAddr>>> + Send>,
        > {
            Box::pin(async { Ok(vec!["127.0.0.1".parse().unwrap()]) })
        }
    }

    async fn discover(server_url: &str, auth_server: String) -> Option<String> {
        discover_via(server_url, false, Arc::new(ResolvesToLoopback), auth_server).await
    }

    async fn discover_via(
        server_url: &str,
        server_on_lan: bool,
        lookup: Arc<dyn crate::services::ssrf_guard::HostLookup>,
        auth_server: String,
    ) -> Option<String> {
        let client =
            super::super::http_client::mcp_oauth_client_with(5, server_url, server_on_lan, lookup);
        McpConnection::discover_auth_metadata_with(
            &client,
            server_url,
            server_on_lan,
            &[auth_server],
        )
        .await
        .map(|metadata| metadata.token_endpoint)
    }

    /// AGE-1101 / RUSTSEC-2026-0457: a public MCP server cannot steer
    /// chatty's OAuth discovery at loopback, by name or by IP literal.
    #[tokio::test]
    async fn oauth_discovery_refuses_loopback_for_a_public_server() {
        let (port, hits) = metadata_server().await;

        let public = "https://mcp.example.com/mcp";
        assert_eq!(
            discover(public, format!("http://auth.example.com:{port}")).await,
            None
        );
        assert_eq!(
            discover(public, format!("http://127.0.0.1:{port}")).await,
            None
        );
        assert_eq!(
            discover(public, format!("http://[::ffff:127.0.0.1]:{port}")).await,
            None
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// A server the user runs on this machine may keep its authorization
    /// server on this machine too.
    #[tokio::test]
    async fn oauth_discovery_reaches_loopback_for_a_loopback_server() {
        let (port, hits) = metadata_server().await;

        let local = "http://localhost:3000/mcp";
        assert_eq!(
            discover(local, format!("http://auth.example.com:{port}")).await,
            Some("https://auth.example.com/token".to_string())
        );
        assert_eq!(
            discover(local, format!("http://127.0.0.1:{port}")).await,
            Some("https://auth.example.com/token".to_string())
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// A server that answers every request with a redirect to `location`.
    async fn redirecting_server(location: String) -> u16 {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        port
    }

    /// AGE-1101: the well-known probe follows no redirect, and a public
    /// server's probe never connects to loopback, even when its name
    /// resolves there.
    #[tokio::test]
    async fn oauth_probe_follows_no_redirect_and_refuses_loopback_for_a_public_server() {
        let (target, hits) = metadata_server().await;

        // A local server redirecting its well-known document elsewhere: the
        // redirect is not followed.
        let redirect = redirecting_server(format!(
            "http://127.0.0.1:{target}/.well-known/oauth-protected-resource"
        ))
        .await;
        let local = format!("http://127.0.0.1:{redirect}/mcp");
        let client = super::super::http_client::mcp_oauth_client_with(
            5,
            &local,
            false,
            Arc::new(ResolvesToLoopback),
        );
        assert_eq!(
            McpConnection::probe_oauth_metadata_with(&client, &local, false).await,
            None
        );

        // A public name that resolves to loopback is refused before connecting.
        let public = format!("http://mcp.example.com:{target}/mcp");
        let client = super::super::http_client::mcp_oauth_client_with(
            5,
            &public,
            false,
            Arc::new(ResolvesToLoopback),
        );
        assert_eq!(
            McpConnection::probe_oauth_metadata_with(&client, &public, false).await,
            None
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// Resolves every name to a fixed address.
    struct ResolvesTo(std::net::IpAddr);

    impl crate::services::ssrf_guard::HostLookup for ResolvesTo {
        fn lookup(
            &self,
            _host: &str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<Vec<std::net::IpAddr>>> + Send>,
        > {
            let ip = self.0;
            Box::pin(async move { Ok(vec![ip]) })
        }
    }

    /// A server configured by a name that resolves to the LAN may keep its
    /// authorization server on the LAN; a public server may not, and
    /// neither may reach loopback (AGE-1101).
    #[tokio::test]
    async fn oauth_discovery_allows_lan_only_for_a_server_on_the_lan() {
        use crate::services::ssrf_guard::mcp_server_on_lan;
        let lan: std::net::IpAddr = "192.168.1.5".parse().unwrap();

        let lan_server = "https://homey.local/mcp";
        assert!(mcp_server_on_lan(lan_server, &ResolvesTo(lan)).await);
        let public_server = "https://mcp.example.com/mcp";
        let public: std::net::IpAddr = "93.184.216.34".parse().unwrap();
        assert!(!mcp_server_on_lan(public_server, &ResolvesTo(public)).await);

        // Nothing listens on a LAN address in a test, so the policy that
        // the client's resolver and literal check share is checked directly.
        use crate::services::ssrf_guard::check_mcp_oauth_url_without_lookup;
        assert!(
            check_mcp_oauth_url_without_lookup(lan_server, true, "http://192.168.1.7/").is_ok()
        );
        assert!(
            check_mcp_oauth_url_without_lookup(public_server, false, "http://192.168.1.7/")
                .is_err()
        );
        assert!(check_mcp_oauth_url_without_lookup(lan_server, true, "http://127.0.0.1/").is_err());

        // Loopback stays refused for a LAN server, by name too.
        let (port, hits) = metadata_server().await;
        assert_eq!(
            discover_via(
                lan_server,
                true,
                Arc::new(ResolvesToLoopback),
                format!("http://auth.example.com:{port}")
            )
            .await,
            None
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
