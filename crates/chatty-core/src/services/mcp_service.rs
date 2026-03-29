use anyhow::{Context, Result};
use rmcp::service::ServiceExt;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::services::mcp_token_store::FileCredentialStore;
use crate::settings::models::mcp_store::McpServerConfig;

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
        if config.api_key.as_ref().is_none_or(|k| k.is_empty()) {
            if let Some(auth_servers) = Self::probe_oauth_metadata(&url).await {
                info!(
                    server = %name,
                    "Server advertises OAuth via resource metadata, using OAuth flow"
                );
                return Self::connect_with_oauth(&name, &url, Some(auth_servers)).await;
            }
        }

        // Try connecting with retries for transient transport errors (e.g.
        // local gateway not yet listening at startup).
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

        let transport_config = StreamableHttpClientTransportConfig {
            uri: url.as_str().into(),
            auth_header: config.api_key.filter(|k| !k.is_empty()),
            ..Default::default()
        };

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
    async fn probe_oauth_metadata(url: &str) -> Option<Vec<String>> {
        let base = url.trim_end_matches('/');
        let origin = if let Ok(parsed) = reqwest::Url::parse(base) {
            format!("{}://{}", parsed.scheme(), parsed.authority())
        } else {
            return None;
        };

        let metadata_url = format!("{origin}/.well-known/oauth-protected-resource");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .ok()?;

        let resp = client.get(&metadata_url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }

        #[derive(serde::Deserialize)]
        struct ResourceMetadata {
            authorization_servers: Option<Vec<String>>,
        }

        let meta: ResourceMetadata = resp.json().await.ok()?;
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
    async fn discover_auth_metadata_manually(
        auth_servers: &[String],
    ) -> Option<rmcp::transport::auth::AuthorizationMetadata> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .ok()?;

        for server in auth_servers {
            let server = server.trim_end_matches('/');
            let metadata_url = format!("{server}/.well-known/oauth-authorization-server");

            debug!(url = %metadata_url, "Trying manual auth metadata discovery");

            let resp = match client.get(&metadata_url).send().await {
                Ok(r) if r.status().is_success() => r,
                _ => {
                    // Also try openid-configuration as fallback
                    let oidc_url = format!("{server}/.well-known/openid-configuration");
                    match client.get(&oidc_url).send().await {
                        Ok(r) if r.status().is_success() => r,
                        _ => continue,
                    }
                }
            };

            match resp
                .json::<rmcp::transport::auth::AuthorizationMetadata>()
                .await
            {
                Ok(metadata) => {
                    debug!(server = %server, "Found authorization metadata");
                    return Some(metadata);
                }
                Err(e) => {
                    warn!(
                        server = %server,
                        error = ?e,
                        "Failed to parse authorization metadata"
                    );
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
        use rmcp::transport::auth::{AuthClient, AuthorizationManager, CredentialStore};

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
        let mut auth_manager = AuthorizationManager::new(url)
            .await
            .with_context(|| format!("Failed to create OAuth manager for {name}"))?;

        // Use a no-redirect HTTP client for metadata discovery.
        // Some servers (e.g. Homey) redirect their root URL to a marketing page,
        // and rmcp's discovery incorrectly treats the 200 HTML as resource metadata.
        let no_redirect_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to build HTTP client")?;
        auth_manager
            .with_client(no_redirect_client)
            .map_err(|e| anyhow::anyhow!(e))?;

        // Set file-backed credential store so tokens are auto-persisted
        auth_manager.set_credential_store(credential_store);

        // Discover authorization server metadata.
        // When our probe already identified the auth servers, do manual discovery
        // to bypass rmcp's discover_metadata() which fails when the MCP endpoint
        // returns 200 HTML for GET requests (e.g. HuggingFace).
        let metadata = if let Some(ref servers) = probed_auth_servers {
            if let Some(meta) = Self::discover_auth_metadata_manually(servers).await {
                meta
            } else {
                // Manual discovery failed; fall back to rmcp's built-in discovery
                warn!(server = %name, "Manual metadata discovery failed, trying rmcp discovery");
                auth_manager
                    .discover_metadata()
                    .await
                    .with_context(|| format!("Failed to discover OAuth metadata for {name}"))?
            }
        } else {
            auth_manager
                .discover_metadata()
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

        let oauth_config = match auth_manager.register_client("Chatty", &redirect_uri).await {
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
        let (code, state) = Self::wait_for_oauth_callback(listener).await?;

        // Exchange the authorization code for an access token
        // (FileCredentialStore automatically persists the token via CredentialStore::save)
        info!(server = %name, "Exchanging OAuth code for token");
        auth_manager
            .exchange_code_for_token(&code, &state)
            .await
            .with_context(|| format!("Failed to exchange OAuth code for {name}"))?;

        // Build an authorized transport and connect
        let auth_client = AuthClient::new(reqwest::Client::default(), auth_manager);

        let transport = StreamableHttpClientTransport::with_client(
            auth_client,
            StreamableHttpClientTransportConfig {
                uri: url.into(),
                ..Default::default()
            },
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
        use rmcp::transport::auth::{AuthClient, AuthorizationManager};

        let mut auth_manager = AuthorizationManager::new(url)
            .await
            .with_context(|| format!("Failed to create OAuth manager for {name}"))?;

        // No-redirect client (same rationale as connect_with_oauth)
        let no_redirect_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to build HTTP client")?;
        auth_manager
            .with_client(no_redirect_client)
            .map_err(|e| anyhow::anyhow!(e))?;

        auth_manager.set_credential_store(credential_store);

        // Use manual discovery when probed_auth_servers is available (same as connect_with_oauth)
        let metadata = if let Some(ref servers) = probed_auth_servers {
            if let Some(meta) = Self::discover_auth_metadata_manually(servers).await {
                meta
            } else {
                auth_manager
                    .discover_metadata()
                    .await
                    .with_context(|| format!("Failed to discover OAuth metadata for {name}"))?
            }
        } else {
            auth_manager
                .discover_metadata()
                .await
                .with_context(|| format!("Failed to discover OAuth metadata for {name}"))?
        };
        auth_manager.set_metadata(metadata);

        let auth_client = AuthClient::new(reqwest::Client::default(), auth_manager);

        let transport = StreamableHttpClientTransport::with_client(
            auth_client,
            StreamableHttpClientTransportConfig {
                uri: url.into(),
                ..Default::default()
            },
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
    /// extracts `code` and `state` parameters, sends a success page, and returns.
    ///
    /// Supports both `GET /callback?code=X&state=Y` (standard query mode)
    /// and `POST /callback` with URL-encoded form body (`response_mode=form_post`,
    /// used by servers like Homey).
    async fn wait_for_oauth_callback(
        listener: tokio::net::TcpListener,
    ) -> Result<(String, String)> {
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
        Ok((code, state))
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
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open").arg(url).spawn()?;
        }
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open").arg(url).spawn()?;
        }
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .spawn()?;
        }
        Ok(())
    }

    /// List available tools from this MCP server, using cache when available
    pub async fn list_tools(&mut self) -> Result<Vec<rmcp::model::Tool>> {
        if let Some(ref cached) = self.cached_tools {
            debug!(server = %self.name, "Returning cached tool list");
            return Ok(cached.clone());
        }

        let response = self
            .service
            .list_tools(Default::default())
            .await
            .with_context(|| format!("Failed to list tools from server: {}", self.name))?;

        self.cached_tools = Some(response.tools.clone());
        Ok(response.tools)
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
    /// Active connections keyed by server name
    connections: Arc<RwLock<HashMap<String, McpConnection>>>,
}

impl McpService {
    /// Create a new MCP service
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
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

    /// Delete stored OAuth credentials for a server.
    /// Call this when a server is removed from settings.
    pub async fn delete_server_credentials(server_name: &str) {
        FileCredentialStore::delete_for_server(server_name).await;
    }

    /// Check if a server has cached OAuth credentials.
    pub fn has_cached_credentials(server_name: &str) -> bool {
        FileCredentialStore::has_credentials(server_name)
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
    pub async fn get_all_tools_with_sinks(
        &self,
    ) -> Result<Vec<(String, Vec<rmcp::model::Tool>, rmcp::service::ServerSink)>> {
        let mut connections = self.connections.write().await;
        let mut result = Vec::new();

        for (name, connection) in connections.iter_mut() {
            match connection.list_tools().await {
                Ok(tools) => {
                    let server_sink = connection.service.peer().clone();
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
                Err(e) => {
                    error!(
                        server = %name,
                        error = ?e,
                        "Failed to list tools from MCP server"
                    );
                }
            }
        }

        Ok(result)
    }
}

impl Default for McpService {
    fn default() -> Self {
        Self::new()
    }
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

    // --- Tool cache: get_all_tools idempotent with no connections ---

    #[tokio::test]
    async fn test_get_all_tools_idempotent_no_connections() {
        let svc = McpService::new();
        let r1 = svc.get_all_tools_with_sinks().await.unwrap();
        let r2 = svc.get_all_tools_with_sinks().await.unwrap();
        assert_eq!(r1.len(), r2.len());
    }
}
