//! Who may reach the gateway (ADR-0021 § 4, EN-0d): the per-launch token
//! every route of every listener serving [`build_router`] requires, the
//! owner-only directory its socket and token file live in, and the Unix
//! socket it is served on. There is no TCP listener.
//!
//! * **Token.** A fresh random bearer per [`ProtocolGateway`], checked on
//!   every request (`Authorization: Bearer <token>`), fallback included. It
//!   is handed to an external MCP client only through a `0600` file next to
//!   the socket, never through argv.
//! * **Directory.** `$XDG_RUNTIME_DIR/chatty-run`, else
//!   `<cache dir>/chatty-run`: never the shared temp dir. It is created
//!   `0700`, and before anything is bound in it, it must be a real
//!   directory (not a symlink) owned by this user with no group or other
//!   bits; otherwise the gateway refuses to start.
//! * **Stale sockets.** A socket left by a crashed run is removed only when
//!   it is a socket this user owns and nothing answers on it.
//!
//! Windows has no owner-only directory yet (an owner-only DACL under
//! `%LOCALAPPDATA%` is the plan, tracked by AGE-778), so there the gateway
//! refuses to start with [`WINDOWS_UNSUPPORTED`]; the broker's direct
//! transport does not need it, so on-desktop plugin tools work on Windows
//! today — only *external* MCP clients reaching the gateway are
//! macOS/Linux-only for now.
//!
//! [`build_router`]: crate::ProtocolGateway::build_router
//! [`ProtocolGateway`]: crate::ProtocolGateway

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

/// Why the gateway does not start on Windows.
pub const WINDOWS_UNSUPPORTED: &str = "the module gateway needs an owner-only socket directory, \
     which chatty only has on Unix so far; external MCP access is macOS/Linux-only for now \
     (ADR-0021 § 4, tracked by AGE-778)";

/// The name of the owner-only directory under the runtime (or cache) dir.
const DIR_NAME: &str = "chatty-run";

// ---------------------------------------------------------------------------
// Token
// ---------------------------------------------------------------------------

/// The gateway's per-launch bearer. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct GatewayToken(Arc<str>);

impl GatewayToken {
    /// A fresh token: two v4 UUIDs from the OS RNG, 244 random bits.
    pub fn generate() -> Self {
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        Self(token.into())
    }

    /// The token itself, for the `Authorization: Bearer` header.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `presented` is this token, in time that does not depend on
    /// where the first differing byte is.
    fn matches(&self, presented: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), presented.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

impl fmt::Debug for GatewayToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GatewayToken(<redacted>)")
    }
}

/// Middleware: refuse a request without `Authorization: Bearer <token>`.
pub(crate) async fn require_token(
    State(token): State<GatewayToken>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if presented.is_some_and(|presented| token.matches(presented)) {
        return next.run(request).await;
    }
    tracing::warn!("refused a gateway request without the launch token");
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(json!({ "error": "unauthorized: the gateway's launch token is required" })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Owner-only directory
// ---------------------------------------------------------------------------

/// Where the gateway's socket and token file go by default:
/// `$XDG_RUNTIME_DIR/chatty-run`, else `<cache dir>/chatty-run`. Unix socket
/// paths are limited to about 100 bytes, so this stays short.
pub fn default_runtime_dir() -> io::Result<PathBuf> {
    if cfg!(windows) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            WINDOWS_UNSUPPORTED,
        ));
    }
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .map(|dir| dir.join(DIR_NAME))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no runtime or cache directory to put the gateway's socket in",
            )
        })
}

/// A directory checked to be this user's alone: a real directory, owned by
/// this user, mode `0700` or tighter. Only [`PrivateDir::open`] makes one.
#[derive(Debug, Clone)]
pub struct PrivateDir(PathBuf);

impl PrivateDir {
    /// Create `path` (`0700`) if it is missing, then check it; refuse it
    /// when it is a symlink, not a directory, owned by someone else, or
    /// open to group or others.
    #[cfg(unix)]
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        check_private(&path, current_uid())?;
        Ok(Self(path))
    }

    /// No owner-only directory on Windows yet (see the module docs).
    #[cfg(not(unix))]
    pub fn open(_path: impl Into<PathBuf>) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            WINDOWS_UNSUPPORTED,
        ))
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// `name` inside it.
    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Write `token` to `name` in this directory, `0600`, replacing a file
    /// this user left there. Returns its path.
    #[cfg(unix)]
    pub fn write_token(&self, name: &str, token: &GatewayToken) -> io::Result<PathBuf> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(token.as_str().as_bytes())?;
        Ok(path)
    }
}

/// This process's effective user id.
#[cfg(unix)]
pub(crate) fn current_uid() -> u32 {
    // SAFETY: `geteuid` cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

/// Refuse `path` unless it is a directory (not a symlink) owned by `uid`
/// with no group or other permission bits.
#[cfg(unix)]
pub(crate) fn check_private(path: &Path, uid: u32) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)?;
    let refuse = |why: String| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing the gateway directory {}: {why}", path.display()),
        ))
    };
    if !meta.file_type().is_dir() {
        return refuse("it is not a directory (or is a symlink)".to_string());
    }
    if meta.uid() != uid {
        return refuse(format!("it is owned by uid {}, not {uid}", meta.uid()));
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o077 != 0 {
        return refuse(format!("its mode is {mode:o}, not 700"));
    }
    Ok(())
}

/// Clear the way for a socket at `path`: nothing there is fine; a socket
/// owned by `uid` that nothing answers on is a crashed run's and is
/// removed. Anything else — a live socket, someone else's socket, a file
/// that is not a socket — is left alone and refused.
#[cfg(unix)]
pub(crate) fn remove_stale_socket(path: &Path, uid: u32) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let refuse = |kind, why: &str| {
        Err(io::Error::new(
            kind,
            format!("not removing {}: {why}", path.display()),
        ))
    };
    if !meta.file_type().is_socket() {
        return refuse(io::ErrorKind::AlreadyExists, "it is not a socket");
    }
    if meta.uid() != uid {
        return refuse(io::ErrorKind::PermissionDenied, "another user owns it");
    }
    // A blocking connect is the probe: it reaches a listener or it does not.
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return refuse(io::ErrorKind::AddrInUse, "something is listening on it");
    }
    tracing::debug!(socket = %path.display(), "Removing a stale socket");
    std::fs::remove_file(path)
}

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

/// A router served on a Unix socket in a [`PrivateDir`], with its token in
/// a `0600` file beside it. Dropping it stops the server and removes both
/// files.
#[cfg(unix)]
pub struct SocketServer {
    socket: PathBuf,
    token_file: PathBuf,
    task: tokio::task::JoinHandle<()>,
    /// Set by the first [`stop`](Self::stop), so a later one never removes
    /// files a newer server put at the same paths.
    stopped: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl SocketServer {
    /// Serve `router` at `<dir>/<name>.sock` and write `token` to
    /// `<dir>/<name>.token`. `router` must be one [`build_router`] made
    /// with `token`.
    ///
    /// [`build_router`]: crate::ProtocolGateway::build_router
    pub fn bind(
        dir: &PrivateDir,
        name: &str,
        router: axum::Router,
        token: &GatewayToken,
    ) -> io::Result<Self> {
        let socket = dir.join(&format!("{name}.sock"));
        remove_stale_socket(&socket, current_uid())?;
        let listener = tokio::net::UnixListener::bind(&socket)?;
        let token_file = match dir.write_token(&format!("{name}.token"), token) {
            Ok(path) => path,
            Err(error) => {
                let _ = std::fs::remove_file(&socket);
                return Err(error);
            }
        };
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });
        Ok(Self {
            socket,
            token_file,
            task,
            stopped: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The socket an MCP client connects to.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// The file holding the token.
    pub fn token_path(&self) -> &Path {
        &self.token_file
    }

    /// Stop serving and remove the socket and the token file. Dropping it
    /// does the same.
    pub fn stop(&self) {
        if self.stopped.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        self.task.abort();
        for path in [&self.socket, &self.token_file] {
            if let Err(error) = std::fs::remove_file(path)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::debug!(path = %path.display(), %error, "Could not remove a gateway file");
            }
        }
    }
}

#[cfg(unix)]
impl Drop for SocketServer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_token_matches_only_itself_and_never_prints() {
        let token = GatewayToken::generate();
        assert!(token.matches(token.as_str()));
        assert!(!token.matches(""));
        assert!(!token.matches(&token.as_str()[1..]));
        assert_ne!(token, GatewayToken::generate());
        assert!(!format!("{token:?}").contains(token.as_str()));
    }

    #[test]
    fn socket_dir_wrong_owner_or_mode_refuses_start() {
        let root = tempfile::tempdir().unwrap();

        // Too open: refused, and left as it was.
        let open = root.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = PrivateDir::open(&open).expect_err("a 0755 directory is refused");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        assert!(error.to_string().contains("755"), "{error}");

        // Someone else's: refused. (Only root can chown, so the check is
        // run with another uid as the expected owner.)
        let mine = root.path().join("mine");
        let dir = PrivateDir::open(&mine).expect("a fresh directory is created 0700");
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let error = check_private(&mine, current_uid() + 1).expect_err("another owner is refused");
        assert!(error.to_string().contains("owned by uid"), "{error}");

        // A symlink to a good directory is refused too.
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&mine, &link).unwrap();
        assert!(PrivateDir::open(&link).is_err());

        // And the gateway itself does not start in the open directory.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut gateway = crate::ProtocolGateway::new(Arc::new(tokio::sync::RwLock::new(
            chatty_module_registry::ModuleRegistry::new(
                Arc::new(NoLlm),
                chatty_wasm_runtime::ResourceLimits::default(),
            )
            .unwrap(),
        )))
        .with_runtime_dir(&open);
        let error = rt
            .block_on(gateway.start())
            .expect_err("the gateway refuses a 0755 directory");
        assert!(format!("{error:#}").contains("755"), "{error:#}");
        assert!(!open.join("gateway.sock").exists());
        assert!(!open.join("gateway.token").exists());
    }

    #[test]
    fn stale_socket_not_owned_is_not_unlinked() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("gateway.sock");
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        assert!(socket.exists(), "a stale socket is left behind");

        // Not ours: left in place.
        let error = remove_stale_socket(&socket, current_uid() + 1)
            .expect_err("another user's socket is refused");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        assert!(
            socket.exists(),
            "another user's socket must not be unlinked"
        );

        // Not a socket: left in place.
        let file = dir.path().join("plain");
        std::fs::write(&file, "x").unwrap();
        assert!(remove_stale_socket(&file, current_uid()).is_err());
        assert!(file.exists());

        // Live and ours: left in place.
        let live = dir.path().join("live.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&live).unwrap();
        let error = remove_stale_socket(&live, current_uid()).expect_err("a live socket stays");
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert!(live.exists());

        // Stale and ours: removed.
        remove_stale_socket(&socket, current_uid()).expect("our stale socket is removed");
        assert!(!socket.exists());
    }

    struct NoLlm;

    impl chatty_wasm_runtime::LlmProvider for NoLlm {
        fn complete(
            &self,
            _: &str,
            _: Vec<chatty_wasm_runtime::Message>,
            _: Option<String>,
        ) -> Result<chatty_wasm_runtime::CompletionResponse, String> {
            Err("no llm".into())
        }
    }
}
