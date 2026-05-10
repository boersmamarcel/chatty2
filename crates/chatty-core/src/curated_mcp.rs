//! Curated catalog of well-known external MCP servers.
//!
//! Ships a small, hand-picked list of public MCP endpoints (Hugging Face,
//! Notion, Atlassian, …) so users can opt into useful integrations with a
//! single click instead of having to look up endpoint URLs themselves.
//!
//! The catalog itself is purely metadata — entries are seeded into the
//! [`ExtensionsModel`] (and the legacy `mcp_servers.json`) on first launch
//! with `enabled = false`. From there they flow through the existing
//! Extensions UI: users toggle them on/off, the state is persisted, and
//! connection / auth status is surfaced through [`McpAuthStatus`].
//!
//! Provider-specific connection details (OAuth flows, transport quirks) are
//! tracked in follow-up issues — see the issue that introduced this catalog.
//!
//! [`ExtensionsModel`]: crate::settings::models::extensions_store::ExtensionsModel
//! [`McpAuthStatus`]: crate::settings::models::mcp_store::McpAuthStatus

use crate::settings::models::extensions_store::{
    ExtensionKind, ExtensionSource, ExtensionsModel, InstalledExtension,
};
use crate::settings::models::mcp_store::McpServerConfig;

/// Transport protocol advertised by an upstream MCP server.
///
/// The MCP client currently only speaks streamable HTTP; this enum is kept
/// as catalog metadata so users (and future transport implementations) can
/// see what the upstream actually serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CuratedMcpTransport {
    /// Streamable HTTP — natively supported by the built-in MCP client.
    StreamableHttp,
    /// Server-Sent Events — connection is best-effort until an SSE transport
    /// is added; users may need to bridge through a local proxy in the
    /// meantime.
    Sse,
}

impl CuratedMcpTransport {
    pub fn as_str(&self) -> &'static str {
        match self {
            CuratedMcpTransport::StreamableHttp => "streamable-http",
            CuratedMcpTransport::Sse => "sse",
        }
    }
}

/// Metadata for a single curated MCP server.
///
/// All fields are static / `'static` because the catalog is compiled in.
#[derive(Clone, Debug)]
pub struct CuratedMcpEntry {
    /// Stable extension id (e.g. `"mcp-huggingface"`). Used as the
    /// [`InstalledExtension::id`] so that toggles persist across restarts
    /// even if display names change.
    pub id: &'static str,
    /// Slug used as the underlying MCP server name (e.g. `"huggingface"`).
    pub slug: &'static str,
    /// Human-readable name shown in the Extensions UI.
    pub display_name: &'static str,
    /// MCP endpoint URL the client connects to.
    pub url: &'static str,
    /// Upstream transport the server advertises.
    pub transport: CuratedMcpTransport,
    /// Short description shown in the UI.
    pub description: &'static str,
    /// Public docs URL — referenced from setup guidance.
    pub docs_url: &'static str,
    /// Brief notes about how authentication works for this provider.
    pub auth_notes: &'static str,
    /// Initial enabled state when seeded. Curated entries default to
    /// `false` so users opt in explicitly.
    pub default_enabled: bool,
}

impl CuratedMcpEntry {
    /// Build the [`McpServerConfig`] that backs this catalog entry.
    pub fn to_mcp_config(&self) -> McpServerConfig {
        McpServerConfig {
            name: self.slug.to_string(),
            url: self.url.to_string(),
            api_key: None,
            enabled: self.default_enabled,
            is_module: false,
        }
    }

    /// Build the user-facing description shown in the Extensions list.
    /// Combines the short description with the docs URL so users know
    /// where to look for setup guidance.
    pub fn ui_description(&self) -> String {
        format!(
            "{} See {} for setup details.",
            self.description, self.docs_url
        )
    }
}

/// The built-in curated MCP catalog.
///
/// To add a provider, append a new [`CuratedMcpEntry`] here and document it
/// in `docs/curated-mcp-catalog.md`.
pub fn curated_catalog() -> &'static [CuratedMcpEntry] {
    &CURATED_CATALOG
}

const CURATED_CATALOG: &[CuratedMcpEntry] = &[
    CuratedMcpEntry {
        id: "mcp-huggingface",
        slug: "huggingface",
        display_name: "Hugging Face",
        url: "https://hf.co/mcp",
        transport: CuratedMcpTransport::StreamableHttp,
        description: "Access Hugging Face Hub models, datasets, and Spaces via the official MCP server.",
        docs_url: "https://huggingface.co/docs/hub/agents-mcp",
        auth_notes: "Optional. Provide a Hugging Face access token as the API key to access private \
             repositories or higher rate limits.",
        default_enabled: false,
    },
    CuratedMcpEntry {
        id: "mcp-notion",
        slug: "notion",
        display_name: "Notion",
        url: "https://mcp.notion.com/sse",
        transport: CuratedMcpTransport::Sse,
        description: "Search and edit Notion pages, databases, and comments through Notion's \
             hosted MCP server.",
        docs_url: "https://developers.notion.com/docs/mcp",
        auth_notes: "OAuth — sign in with your Notion workspace when prompted by the MCP server. \
             The hosted endpoint serves Server-Sent Events (SSE); pair with an SSE-capable \
             transport bridge if the built-in streamable-HTTP client cannot reach it directly.",
        default_enabled: false,
    },
    CuratedMcpEntry {
        id: "mcp-atlassian",
        slug: "atlassian",
        display_name: "Atlassian (Jira + Confluence)",
        url: "https://mcp.atlassian.com/v1/sse",
        transport: CuratedMcpTransport::Sse,
        description: "Search issues, comment on tickets, and read Confluence pages via \
             Atlassian's official Remote MCP server.",
        docs_url: "https://www.atlassian.com/platform/remote-mcp-server",
        auth_notes: "OAuth — Atlassian Cloud sign-in is performed in the browser on first connect. \
             The hosted endpoint serves Server-Sent Events (SSE); pair with an SSE-capable \
             transport bridge if the built-in streamable-HTTP client cannot reach it directly.",
        default_enabled: false,
    },
    CuratedMcpEntry {
        id: "mcp-google-calendar",
        slug: "google-calendar",
        display_name: "Google Calendar",
        url: "https://calendarmcp.googleapis.com/mcp/v1",
        transport: CuratedMcpTransport::StreamableHttp,
        description: "Read and manage Google Calendar events via Google's official MCP server.",
        docs_url: "https://developers.google.com/calendar",
        auth_notes: "OAuth — sign in with your Google account when prompted. The server uses Google \
             OAuth 2.0; ensure the Calendar API scope is granted.",
        default_enabled: false,
    },
    CuratedMcpEntry {
        id: "mcp-gmail",
        slug: "gmail",
        display_name: "Gmail",
        url: "https://gmailmcp.googleapis.com/mcp/v1",
        transport: CuratedMcpTransport::StreamableHttp,
        description: "Read, search, and send Gmail messages via Google's official MCP server.",
        docs_url: "https://developers.google.com/gmail",
        auth_notes: "OAuth — sign in with your Google account when prompted. The server uses Google \
             OAuth 2.0; ensure the Gmail API scope is granted.",
        default_enabled: false,
    },
    CuratedMcpEntry {
        id: "mcp-google-drive",
        slug: "google-drive",
        display_name: "Google Drive",
        url: "https://drivemcp.googleapis.com/mcp/v1",
        transport: CuratedMcpTransport::StreamableHttp,
        description: "Browse, search, and manage files in Google Drive via Google's official MCP server.",
        docs_url: "https://developers.google.com/drive",
        auth_notes: "OAuth — sign in with your Google account when prompted. The server uses Google \
             OAuth 2.0; ensure the Drive API scope is granted.",
        default_enabled: false,
    },
];

/// Ensure every entry in [`curated_catalog`] is present in the extensions
/// model and the legacy MCP server list. Idempotent: existing entries
/// (matched by [`CuratedMcpEntry::id`]) are left untouched, preserving any
/// user-set `enabled` flag or API key.
///
/// Returns `true` if at least one new entry was added — callers should then
/// persist `extensions` and `mcp_servers`.
pub fn ensure_curated_mcp_servers(
    extensions: &mut ExtensionsModel,
    mcp_servers: &mut Vec<McpServerConfig>,
) -> bool {
    let mut changed = false;

    for entry in curated_catalog() {
        if extensions.is_installed(entry.id) {
            continue;
        }

        let config = entry.to_mcp_config();

        extensions.add(InstalledExtension {
            id: entry.id.to_string(),
            display_name: entry.display_name.to_string(),
            description: entry.ui_description(),
            kind: ExtensionKind::McpServer(config.clone()),
            source: ExtensionSource::Hive {
                module_name: entry.slug.to_string(),
                version: "curated".to_string(),
            },
            pricing_model: None,
            enabled: entry.default_enabled,
        });

        if !mcp_servers.iter().any(|s| s.name == config.name) {
            mcp_servers.push(config);
        }

        changed = true;
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_initial_providers() {
        let ids: Vec<_> = curated_catalog().iter().map(|e| e.id).collect();
        assert!(ids.contains(&"mcp-huggingface"));
        assert!(ids.contains(&"mcp-notion"));
        assert!(ids.contains(&"mcp-atlassian"));
        assert!(ids.contains(&"mcp-google-calendar"));
        assert!(ids.contains(&"mcp-gmail"));
        assert!(ids.contains(&"mcp-google-drive"));
    }

    #[test]
    fn catalog_entries_have_required_metadata() {
        for entry in curated_catalog() {
            assert!(!entry.id.is_empty(), "id missing");
            assert!(!entry.slug.is_empty(), "slug missing for {}", entry.id);
            assert!(
                !entry.display_name.is_empty(),
                "display_name missing for {}",
                entry.id
            );
            assert!(
                entry.url.starts_with("https://") || entry.url.starts_with("http://"),
                "url must be http(s) for {}",
                entry.id
            );
            assert!(
                !entry.description.is_empty(),
                "description missing for {}",
                entry.id
            );
            assert!(
                entry.docs_url.starts_with("https://") || entry.docs_url.starts_with("http://"),
                "docs_url must be http(s) for {}",
                entry.id
            );
            assert!(
                !entry.auth_notes.is_empty(),
                "auth_notes missing for {}",
                entry.id
            );
        }
    }

    #[test]
    fn catalog_ids_and_slugs_are_unique() {
        let mut ids: Vec<_> = curated_catalog().iter().map(|e| e.id).collect();
        let id_count = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), id_count, "duplicate ids in catalog");

        let mut slugs: Vec<_> = curated_catalog().iter().map(|e| e.slug).collect();
        let slug_count = slugs.len();
        slugs.sort();
        slugs.dedup();
        assert_eq!(slugs.len(), slug_count, "duplicate slugs in catalog");
    }

    #[test]
    fn curated_entries_default_to_disabled() {
        for entry in curated_catalog() {
            assert!(
                !entry.default_enabled,
                "{} should default to disabled so users opt in explicitly",
                entry.id
            );
        }
    }

    #[test]
    fn ensure_seeds_all_entries_on_first_run() {
        let mut extensions = ExtensionsModel::default();
        let mut servers = vec![];

        let added = ensure_curated_mcp_servers(&mut extensions, &mut servers);
        assert!(added);

        for entry in curated_catalog() {
            assert!(extensions.is_installed(entry.id), "{} not seeded", entry.id);
            assert!(
                servers.iter().any(|s| s.name == entry.slug),
                "{} not added to mcp_servers list",
                entry.slug
            );
        }
    }

    #[test]
    fn ensure_is_idempotent() {
        let mut extensions = ExtensionsModel::default();
        let mut servers = vec![];

        ensure_curated_mcp_servers(&mut extensions, &mut servers);
        let count_after_first = extensions.extensions.len();
        let servers_after_first = servers.len();

        let added_again = ensure_curated_mcp_servers(&mut extensions, &mut servers);
        assert!(!added_again);
        assert_eq!(extensions.extensions.len(), count_after_first);
        assert_eq!(servers.len(), servers_after_first);
    }

    #[test]
    fn ensure_preserves_user_enabled_state() {
        let mut extensions = ExtensionsModel::default();
        let mut servers = vec![];

        ensure_curated_mcp_servers(&mut extensions, &mut servers);

        // User toggles Hugging Face on.
        let ext = extensions.find_mut("mcp-huggingface").unwrap();
        ext.enabled = true;

        // Re-running ensure must not reset that flag.
        ensure_curated_mcp_servers(&mut extensions, &mut servers);
        assert!(extensions.find("mcp-huggingface").unwrap().enabled);
    }
}
