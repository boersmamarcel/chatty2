//! Attachments and cloud outputs through the Hive files API (AGE-2094,
//! HX-15).
//!
//! * **Up.** [`upload_attachments`] sends each local attachment to
//!   `POST /api/files`, bound to the conversation (`conversation=<id>` attaches
//!   the new version as one of its inputs), and remembers which server file a
//!   path became in a [`FileMap`], so a later send does not upload it again.
//!   Message text is never rewritten: the server keeps files on the
//!   conversation, not in message content, and renders attachments from
//!   `GET /api/conversations/{id}/files`.
//! * **Down.** [`download_files`] lists that same route and brings every file
//!   this device does not hold into a cache directory, outputs first in
//!   mind: the server deletes a run's outputs after 30 days unless promoted,
//!   so this runs when a cloud conversation is opened, not lazily.
//!
//! Every local write goes through [`write_no_follow`]; a symlink planted at a
//! cache path is refused, never written through. Every request goes through
//! [`send_to_server`] (secure-URL gate, Hive bearer, one refresh on 401).
//!
//! The [`FileMap`] is kept next to the files it describes, as `files.json` in
//! the cache directory the caller passes, one directory per conversation. It
//! is a cache of what the server holds: losing it costs a re-upload, which
//! the server deduplicates by content, never data.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use hive_client::HiveSession;
use hive_client::net::{CappedReadError, Overflow, read_capped};
use serde::{Deserialize, Serialize};

use crate::repositories::conversation_repository::BoxFuture;
use crate::services::no_follow_write::write_no_follow;
use crate::session::hosted::{SIGN_IN_TO_HIVE, send_to_server};

/// The largest file this client uploads or downloads. The server has its own
/// limits; this keeps a stray multi-gigabyte attachment out of memory.
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

/// The map's file name inside a conversation's cache directory.
const MAP_FILE: &str = "files.json";

/// Why a file transfer failed.
#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    /// 401 after the session's one refresh.
    #[error("{SIGN_IN_TO_HIVE}")]
    SignedOut,
    /// 503: the deployment has no file store.
    #[error("the server has no file store")]
    NoFileStore,
    /// 404: the conversation or file is gone.
    #[error("the server does not have that file or conversation")]
    NotFound,
    /// A cache path that must not be written (a symlink, a directory).
    #[error("refused to write {path}: {reason}")]
    UnsafePath { path: String, reason: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// ── Wire ─────────────────────────────────────────────────────────────────────

/// `POST /api/files` (201): the file, with its newest version. Read
/// leniently: the server may add fields.
#[derive(Clone, Debug, Deserialize)]
pub struct UploadedFile {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub media_type: String,
    pub head: FileVersion,
}

/// A version of a file.
#[derive(Clone, Debug, Deserialize)]
pub struct FileVersion {
    pub id: String,
    #[serde(default)]
    pub size: i64,
}

/// Which way a file runs between the conversation and the agent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    Input,
    Output,
    #[default]
    #[serde(other)]
    Other,
}

/// One entry of `GET /api/conversations/{id}/files`: inputs first, then
/// outputs, each by `created_at`.
#[derive(Clone, Debug, Deserialize)]
pub struct ConversationFile {
    pub file: String,
    pub version: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub media_type: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub role: FileRole,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// The server file a local path became.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub file: String,
    pub version: String,
}

/// What the files API offers, over HTTP ([`HttpHiveFilesApi`]) or scripted.
pub trait HiveFilesApi: Send + Sync {
    /// Upload `bytes` as `name`, attached to conversation `remote_id` as an
    /// input.
    fn upload(
        &self,
        remote_id: &str,
        name: &str,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'static, Result<UploadedFile, FilesError>>;

    /// The conversation's inputs and outputs.
    fn list(
        &self,
        remote_id: &str,
    ) -> BoxFuture<'static, Result<Vec<ConversationFile>, FilesError>>;

    /// One version's bytes, at most [`MAX_FILE_BYTES`].
    fn download(
        &self,
        file: &str,
        version: &str,
    ) -> BoxFuture<'static, Result<Vec<u8>, FilesError>>;
}

/// [`HiveFilesApi`] over HTTP.
#[derive(Clone)]
pub struct HttpHiveFilesApi {
    http: reqwest::Client,
    server_url: String,
    auth: Option<Arc<HiveSession>>,
}

impl HttpHiveFilesApi {
    pub fn new(server_url: &str, auth: Option<Arc<HiveSession>>) -> Self {
        Self::with_client(reqwest::Client::new(), server_url, auth)
    }

    pub fn with_client(
        http: reqwest::Client,
        server_url: &str,
        auth: Option<Arc<HiveSession>>,
    ) -> Self {
        Self {
            http,
            server_url: server_url.trim_end_matches('/').to_string(),
            auth,
        }
    }
}

fn status_error(status: reqwest::StatusCode, url: &str) -> FilesError {
    match status {
        reqwest::StatusCode::UNAUTHORIZED => FilesError::SignedOut,
        reqwest::StatusCode::NOT_FOUND => FilesError::NotFound,
        reqwest::StatusCode::SERVICE_UNAVAILABLE => FilesError::NoFileStore,
        other => anyhow::anyhow!("the server answered {other} for {url}").into(),
    }
}

impl HiveFilesApi for HttpHiveFilesApi {
    fn upload(
        &self,
        remote_id: &str,
        name: &str,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'static, Result<UploadedFile, FilesError>> {
        let url = format!("{}/api/files", self.server_url);
        let (http, auth) = (self.http.clone(), self.auth.clone());
        let (remote_id, name, media_type) = (
            remote_id.to_string(),
            name.to_string(),
            media_type.to_string(),
        );
        Box::pin(async move {
            let response = send_to_server(auth.as_deref(), &url, || {
                // Text fields first, the file last, as the server reads them.
                let part = reqwest::multipart::Part::bytes(bytes.clone())
                    .file_name(name.clone())
                    .mime_str(&media_type)
                    .unwrap_or_else(|_| {
                        reqwest::multipart::Part::bytes(bytes.clone()).file_name(name.clone())
                    });
                let form = reqwest::multipart::Form::new()
                    .text("conversation", remote_id.clone())
                    .part("file", part);
                http.post(&url).multipart(form)
            })
            .await?;
            if !response.status().is_success() {
                return Err(status_error(response.status(), &url));
            }
            response
                .json::<UploadedFile>()
                .await
                .map_err(|error| anyhow::anyhow!("unexpected reply from {url}: {error}").into())
        })
    }

    fn list(
        &self,
        remote_id: &str,
    ) -> BoxFuture<'static, Result<Vec<ConversationFile>, FilesError>> {
        let url = format!("{}/api/conversations/{remote_id}/files", self.server_url);
        let (http, auth) = (self.http.clone(), self.auth.clone());
        Box::pin(async move {
            let response = send_to_server(auth.as_deref(), &url, || http.get(&url)).await?;
            if !response.status().is_success() {
                return Err(status_error(response.status(), &url));
            }
            response
                .json::<Vec<ConversationFile>>()
                .await
                .map_err(|error| anyhow::anyhow!("unexpected reply from {url}: {error}").into())
        })
    }

    fn download(
        &self,
        file: &str,
        version: &str,
    ) -> BoxFuture<'static, Result<Vec<u8>, FilesError>> {
        let url = format!(
            "{}/api/files/{file}/versions/{version}/content",
            self.server_url
        );
        let (http, auth) = (self.http.clone(), self.auth.clone());
        Box::pin(async move {
            let response = send_to_server(auth.as_deref(), &url, || http.get(&url)).await?;
            if !response.status().is_success() {
                return Err(status_error(response.status(), &url));
            }
            read_capped(
                response.bytes_stream(),
                MAX_FILE_BYTES,
                Overflow::Error,
                |_| {},
            )
            .await
            .map(|body| body.bytes)
            .map_err(|error| match error {
                CappedReadError::Overflow => {
                    anyhow::anyhow!("{url} is larger than {MAX_FILE_BYTES} bytes").into()
                }
                CappedReadError::Stream(error) => {
                    anyhow::anyhow!("reading {url} failed: {error}").into()
                }
            })
        })
    }
}

// ── The map ──────────────────────────────────────────────────────────────────

/// Which server file each local path is, for one conversation: uploads by
/// path (so a send does not repeat them), downloads by version (so an open
/// does not fetch them again).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMap {
    /// Local path → what it was uploaded as.
    #[serde(default)]
    pub uploaded: BTreeMap<PathBuf, FileRef>,
    /// Version id → the cached path it was downloaded to.
    #[serde(default)]
    pub downloaded: BTreeMap<String, PathBuf>,
}

impl FileMap {
    /// Read the map in `dir`; a missing or unreadable one is empty.
    pub fn load(dir: &Path) -> Self {
        std::fs::read(dir.join(MAP_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Write the map into `dir` without following a symlink.
    pub fn save(&self, dir: &Path) -> Result<(), FilesError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(anyhow::Error::from)?;
        write_cache_file(&dir.join(MAP_FILE), &bytes)
    }
}

fn write_cache_file(target: &Path, bytes: &[u8]) -> Result<(), FilesError> {
    write_no_follow(target, bytes)
        .map(|_| ())
        .map_err(|error| FilesError::UnsafePath {
            path: target.display().to_string(),
            reason: error.to_string(),
        })
}

/// A directory this client may put cache files in: made if absent, and
/// refused when it is, or sits below, a symlink the cache did not make.
fn ensure_cache_dir(dir: &Path) -> Result<(), FilesError> {
    let unsafe_dir = |reason: &str| FilesError::UnsafePath {
        path: dir.display().to_string(),
        reason: reason.to_string(),
    };
    std::fs::create_dir_all(dir).map_err(|error| unsafe_dir(&error.to_string()))?;
    let meta = std::fs::symlink_metadata(dir).map_err(|error| unsafe_dir(&error.to_string()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(unsafe_dir("not a plain directory"));
    }
    Ok(())
}

/// Where conversation `remote_id`'s files are kept on this machine:
/// `<data dir>/chatty/hive-files/<remote id>`. `None` when the platform has no
/// data directory.
pub fn cache_dir(remote_id: &str) -> Option<PathBuf> {
    Some(
        dirs::data_dir()?
            .join("chatty")
            .join("hive-files")
            .join(safe_name(remote_id)),
    )
}

/// A server-supplied name as one safe path component.
fn safe_name(name: &str) -> String {
    let base = Path::new(&name.replace('\\', "/"))
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str().map(str::to_string),
            _ => None,
        })
        .next_back()
        .unwrap_or_default();
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

// ── Up ───────────────────────────────────────────────────────────────────────

/// What [`upload_attachments`] did for one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uploaded {
    pub path: PathBuf,
    pub file: FileRef,
    /// `false` when the map already had it and nothing was sent.
    pub sent: bool,
}

/// Make sure every path in `paths` is on the server as an input of
/// conversation `remote_id`, and return what each became, in order.
///
/// A path the map already knows is not sent again. The map is read from and
/// saved to `cache_dir` (created when absent). The first failure ends the
/// call; what was uploaded before it stays in the map.
///
/// This is the seam a hand-off to the cloud uses to carry a conversation's
/// attachments (HX-14).
pub async fn upload_attachments<A: HiveFilesApi + ?Sized>(
    api: &A,
    remote_id: &str,
    paths: &[PathBuf],
    cache_dir: &Path,
) -> Result<Vec<Uploaded>, FilesError> {
    ensure_cache_dir(cache_dir)?;
    let mut map = FileMap::load(cache_dir);
    let mut done = Vec::with_capacity(paths.len());
    let mut failure = None;
    for path in paths {
        if let Some(file) = map.uploaded.get(path) {
            done.push(Uploaded {
                path: path.clone(),
                file: file.clone(),
                sent: false,
            });
            continue;
        }
        match upload_one(api, remote_id, path).await {
            Ok(file) => {
                map.uploaded.insert(path.clone(), file.clone());
                done.push(Uploaded {
                    path: path.clone(),
                    file,
                    sent: true,
                });
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    map.save(cache_dir)?;
    match failure {
        Some(error) => Err(error),
        None => Ok(done),
    }
}

async fn upload_one<A: HiveFilesApi + ?Sized>(
    api: &A,
    remote_id: &str,
    path: &Path,
) -> Result<FileRef, FilesError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|error| anyhow::anyhow!("cannot read attachment {}: {error}", path.display()))?;
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(anyhow::anyhow!(
            "attachment {} is larger than {MAX_FILE_BYTES} bytes",
            path.display()
        )
        .into());
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| anyhow::anyhow!("cannot read attachment {}: {error}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment");
    let media_type = mime_for(path);
    let uploaded = api.upload(remote_id, name, media_type, bytes).await?;
    Ok(FileRef {
        file: uploaded.id,
        version: uploaded.head.id,
    })
}

/// The media type to declare for an attachment, by extension; the server
/// sniffs the rest.
fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("txt" | "md") => "text/plain",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// Uploads a conversation's attachments when it is handed to the cloud
/// (HX-14's seam): every file its messages name that still exists on this
/// machine, bound to the conversation `remote_id` on the server.
///
/// It is the [`AttachmentUploader`](crate::session::AttachmentUploader) that
/// "Send to the cloud" uses.
pub struct HiveAttachmentUploader {
    files: Arc<dyn HiveFilesApi>,
}

impl HiveAttachmentUploader {
    pub fn new(server_url: &str, auth: Option<Arc<HiveSession>>) -> Self {
        Self::with_api(Arc::new(HttpHiveFilesApi::new(server_url, auth)))
    }

    pub fn with_api(files: Arc<dyn HiveFilesApi>) -> Self {
        Self { files }
    }

    /// Upload the attachments of `conversation` and return it unchanged:
    /// message text is not rewritten, because the server keeps files on the
    /// conversation. A file that is gone from this machine is skipped; any
    /// server failure is an error and the hand-off should stop.
    pub async fn upload_attachments(
        &self,
        remote_id: &str,
        conversation: crate::repositories::ConversationData,
    ) -> anyhow::Result<crate::repositories::ConversationData> {
        let named: Vec<Vec<PathBuf>> =
            serde_json::from_str(&conversation.attachment_paths).unwrap_or_default();
        let mut paths: Vec<PathBuf> = Vec::new();
        for path in named.into_iter().flatten() {
            if paths.contains(&path) {
                continue;
            }
            if path.is_file() {
                paths.push(path);
            } else {
                tracing::warn!(path = %path.display(), "Skipping an attachment that is no longer on this machine");
            }
        }
        if paths.is_empty() {
            return Ok(conversation);
        }
        let dir = cache_dir(remote_id)
            .ok_or_else(|| anyhow::anyhow!("this platform has no data directory for files"))?;
        upload_attachments(&*self.files, remote_id, &paths, &dir).await?;
        Ok(conversation)
    }
}

impl crate::session::AttachmentUploader for HiveAttachmentUploader {
    fn upload_attachments<'a>(
        &'a self,
        _server: &'a crate::session::HandoffServer,
        remote_id: &'a str,
        conversation: crate::repositories::ConversationData,
    ) -> crate::repositories::conversation_repository::BoxFuture<
        'a,
        anyhow::Result<crate::repositories::ConversationData>,
    > {
        Box::pin(HiveAttachmentUploader::upload_attachments(
            self,
            remote_id,
            conversation,
        ))
    }
}

// ── Down ─────────────────────────────────────────────────────────────────────

/// A file brought into the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Downloaded {
    pub name: String,
    pub role: FileRole,
    pub path: PathBuf,
}

/// Bring the files of conversation `remote_id` that this device does not hold
/// into `cache_dir`, and return the ones fetched now.
///
/// Each lands at `<cache_dir>/<version>/<name>`; the name is the server's,
/// reduced to one safe path component. A version already in the map whose
/// file still exists is skipped, and so is a file this device uploaded
/// itself (its bytes are the local attachment). The first failure ends the
/// call, keeping what was fetched before it.
pub async fn download_files<A: HiveFilesApi + ?Sized>(
    api: &A,
    remote_id: &str,
    cache_dir: &Path,
) -> Result<Vec<Downloaded>, FilesError> {
    let listed = api.list(remote_id).await?;
    ensure_cache_dir(cache_dir)?;
    let mut map = FileMap::load(cache_dir);
    let mut fetched = Vec::new();
    let mut failure = None;
    for entry in listed {
        let ours = map.uploaded.values().any(|r| r.version == entry.version);
        let cached = map
            .downloaded
            .get(&entry.version)
            .is_some_and(|path| path.is_file());
        if ours || cached {
            continue;
        }
        match download_one(api, &entry, cache_dir).await {
            Ok(path) => {
                map.downloaded.insert(entry.version.clone(), path.clone());
                fetched.push(Downloaded {
                    name: entry.name.clone(),
                    role: entry.role,
                    path,
                });
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    map.save(cache_dir)?;
    match failure {
        Some(error) => Err(error),
        None => Ok(fetched),
    }
}

/// An output of a cloud run that is on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputFile {
    pub name: String,
    pub path: PathBuf,
    /// When the server made it, unix seconds; `None` when it sent no
    /// readable time.
    pub created_at: Option<i64>,
}

/// The outputs (`role = "output"`) of conversation `remote_id` that are in
/// `cache_dir`, oldest first. Call after [`download_files`]: an output that
/// is not cached (a failed download, a file gone from disk) is left out.
pub async fn cached_outputs<A: HiveFilesApi + ?Sized>(
    api: &A,
    remote_id: &str,
    cache_dir: &Path,
) -> Result<Vec<OutputFile>, FilesError> {
    let listed = api.list(remote_id).await?;
    let map = FileMap::load(cache_dir);
    let mut outputs: Vec<OutputFile> = listed
        .into_iter()
        .filter(|entry| entry.role == FileRole::Output)
        .filter_map(|entry| {
            let path = map.downloaded.get(&entry.version).filter(|p| p.is_file())?;
            Some(OutputFile {
                name: entry.name,
                path: path.clone(),
                created_at: entry
                    .created_at
                    .as_deref()
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                    .map(|at| at.timestamp()),
            })
        })
        .collect();
    outputs.sort_by_key(|output| output.created_at);
    Ok(outputs)
}

/// Which turn each output hangs on: the first turn that ended at or after
/// the output's `created_at`, else the last turn (the run is still going, or
/// the clocks disagree). `turn_ends` is `(history index, end time in unix
/// seconds)` for each assistant turn, in order; the result maps a history
/// index to its outputs' paths.
pub fn outputs_by_turn(
    outputs: &[OutputFile],
    turn_ends: &[(usize, Option<i64>)],
) -> BTreeMap<usize, Vec<PathBuf>> {
    let mut by_turn: BTreeMap<usize, Vec<PathBuf>> = BTreeMap::new();
    let Some(&(last, _)) = turn_ends.last() else {
        return by_turn;
    };
    for output in outputs {
        let turn = output
            .created_at
            .and_then(|at| {
                turn_ends
                    .iter()
                    .find(|(_, end)| end.is_some_and(|end| end >= at))
            })
            .map_or(last, |(index, _)| *index);
        by_turn.entry(turn).or_default().push(output.path.clone());
    }
    by_turn
}

async fn download_one<A: HiveFilesApi + ?Sized>(
    api: &A,
    entry: &ConversationFile,
    cache_dir: &Path,
) -> Result<PathBuf, FilesError> {
    // The version id is the server's too: one component, nothing else.
    let version_dir = cache_dir.join(safe_name(&entry.version));
    ensure_cache_dir(&version_dir)?;
    let target = version_dir.join(safe_name(&entry.name));
    let bytes = api.download(&entry.file, &entry.version).await?;
    write_cache_file(&target, &bytes)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header_regex, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn signed_in(server: &MockServer) -> Arc<HiveSession> {
        Arc::new(HiveSession::new(
            server.uri(),
            Some(hive_client::TokenPair {
                token: "tok".into(),
                refresh_token: "r".into(),
                expires_at: chrono::Utc::now() + chrono::TimeDelta::hours(1),
            }),
        ))
    }

    fn file_json(id: &str, version: &str, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "workspace": "w", "name": name, "media_type": "image/png",
            "head": {"id": version, "size": 3}, "extra": "ignored"
        })
    }

    #[tokio::test]
    async fn sending_uploads_each_attachment_bound_to_the_conversation_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/files"))
            .and(header_regex("authorization", "Bearer tok"))
            .and(header_regex("content-type", "multipart/form-data"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(file_json("f-1", "v-1", "pic.png")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let api = HttpHiveFilesApi::new(&server.uri(), Some(signed_in(&server)));
        let work = tempfile::tempdir().unwrap();
        let png = work.path().join("pic.png");
        std::fs::write(&png, [1u8, 2, 3]).unwrap();
        let cache = work.path().join("cache");

        let first = upload_attachments(&api, "r-1", std::slice::from_ref(&png), &cache)
            .await
            .unwrap();
        assert_eq!(
            first,
            vec![Uploaded {
                path: png.clone(),
                file: FileRef {
                    file: "f-1".into(),
                    version: "v-1".into()
                },
                sent: true
            }]
        );
        // The same path on the next send is not uploaded again (the mock
        // expects exactly one request).
        let again = upload_attachments(&api, "r-1", std::slice::from_ref(&png), &cache)
            .await
            .unwrap();
        assert!(!again[0].sent);

        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&requests[0].body).to_string();
        assert!(body.contains("name=\"conversation\"\r\n\r\nr-1"), "{body}");
        assert!(body.contains("filename=\"pic.png\""), "{body}");
        assert!(
            body.find("name=\"conversation\"") < body.find("name=\"file\""),
            "text fields come before the file"
        );
    }

    #[tokio::test]
    async fn opening_downloads_the_files_this_device_lacks() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/r-1/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"file": "f-1", "version": "v-1", "name": "../../evil.txt", "role": "input"},
                {"file": "f-2", "version": "v-2", "name": "report.md", "role": "output", "size": 5}
            ])))
            .mount(&server)
            .await;
        for (file, version, body) in [("f-1", "v-1", "mine"), ("f-2", "v-2", "hello")] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/api/files/{file}/versions/{version}/content"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
                .mount(&server)
                .await;
        }
        let api = HttpHiveFilesApi::new(&server.uri(), Some(signed_in(&server)));
        let cache = tempfile::tempdir().unwrap();

        let fetched = download_files(&api, "r-1", cache.path()).await.unwrap();
        assert_eq!(fetched.len(), 2);
        let output = fetched.iter().find(|f| f.role == FileRole::Output).unwrap();
        assert_eq!(output.path, cache.path().join("v-2").join("report.md"));
        assert_eq!(std::fs::read(&output.path).unwrap(), b"hello");
        // A hostile name stays inside its version directory.
        let input = fetched.iter().find(|f| f.role == FileRole::Input).unwrap();
        assert_eq!(input.path, cache.path().join("v-1").join("evil.txt"));

        // Opening again fetches nothing.
        let again = download_files(&api, "r-1", cache.path()).await.unwrap();
        assert!(again.is_empty());
    }

    #[tokio::test]
    async fn outputs_hang_on_the_turn_that_ended_at_or_after_their_creation() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/r-1/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"file": "f-1", "version": "v-1", "name": "in.csv", "role": "input",
                 "created_at": "2026-11-26T10:00:00Z"},
                {"file": "f-2", "version": "v-2", "name": "first.md", "role": "output",
                 "created_at": "2026-11-26T10:05:00Z"},
                {"file": "f-3", "version": "v-3", "name": "second.md", "role": "output",
                 "created_at": "2026-11-26T11:30:00Z"}
            ])))
            .mount(&server)
            .await;
        for (file, version) in [("f-1", "v-1"), ("f-2", "v-2"), ("f-3", "v-3")] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/api/files/{file}/versions/{version}/content"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_bytes("x"))
                .mount(&server)
                .await;
        }
        let api = HttpHiveFilesApi::new(&server.uri(), Some(signed_in(&server)));
        let cache = tempfile::tempdir().unwrap();
        download_files(&api, "r-1", cache.path()).await.unwrap();

        let outputs = cached_outputs(&api, "r-1", cache.path()).await.unwrap();
        assert_eq!(
            outputs.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
            ["first.md", "second.md"],
            "inputs are not outputs"
        );

        let at = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
        // Turns at history indexes 1 and 3; the second ended before the
        // last output, so that one lands on the last turn.
        let ends = [
            (1, Some(at("2026-11-26T10:10:00Z"))),
            (3, Some(at("2026-11-26T11:00:00Z"))),
        ];
        let by_turn = outputs_by_turn(&outputs, &ends);
        assert_eq!(by_turn[&1], vec![outputs[0].path.clone()]);
        assert_eq!(by_turn[&3], vec![outputs[1].path.clone()]);
        assert!(outputs_by_turn(&outputs, &[]).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_cache_path_is_refused_and_nothing_is_written_through_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/r-1/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"file": "f-1", "version": "v-1", "name": "report.md", "role": "output"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/files/f-1/versions/v-1/content"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes("payload"))
            .mount(&server)
            .await;
        let api = HttpHiveFilesApi::new(&server.uri(), None);
        let work = tempfile::tempdir().unwrap();
        let victim = work.path().join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();

        // A link at the file's own path.
        let cache = work.path().join("cache");
        std::fs::create_dir_all(cache.join("v-1")).unwrap();
        std::os::unix::fs::symlink(&victim, cache.join("v-1").join("report.md")).unwrap();
        let error = download_files(&api, "r-1", &cache).await.unwrap_err();
        assert!(matches!(error, FilesError::UnsafePath { .. }), "{error:?}");
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");

        // A link where the version directory should be.
        let cache = work.path().join("cache2");
        std::fs::create_dir_all(&cache).unwrap();
        let elsewhere = work.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, cache.join("v-1")).unwrap();
        let error = download_files(&api, "r-1", &cache).await.unwrap_err();
        assert!(matches!(error, FilesError::UnsafePath { .. }), "{error:?}");
        assert!(!elsewhere.join("report.md").exists());
    }

    #[test]
    fn server_names_become_one_safe_component() {
        assert_eq!(safe_name("a/b/c.txt"), "c.txt");
        assert_eq!(safe_name("..\\..\\x"), "x");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name("ok name.pdf"), "ok name.pdf");
    }

    #[tokio::test]
    async fn a_missing_file_store_and_a_signed_out_user_are_typed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/files"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/conversations/r-1/files"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let api = HttpHiveFilesApi::new(&server.uri(), None);

        assert!(matches!(
            api.upload("r-1", "a.txt", "text/plain", vec![1]).await,
            Err(FilesError::NoFileStore)
        ));
        assert!(matches!(api.list("r-1").await, Err(FilesError::SignedOut)));
    }

    #[tokio::test]
    async fn a_hand_off_uploads_the_files_its_messages_name_and_skips_the_missing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/files"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(file_json("f-1", "v-1", "a.png")),
            )
            .expect(1)
            .mount(&server)
            .await;
        let work = tempfile::tempdir().unwrap();
        let there = work.path().join("a.png");
        std::fs::write(&there, [1u8]).unwrap();
        let gone = work.path().join("gone.png");
        let mut row =
            crate::services::hive_sync::ConversationDetail::default().to_conversation_data("local");
        row.attachment_paths =
            serde_json::to_string(&vec![vec![there.clone(), gone], vec![there], Vec::new()])
                .unwrap();

        let uploader = HiveAttachmentUploader::new(&server.uri(), None);
        let handed = uploader
            .upload_attachments("r-hand-off-test", row.clone())
            .await
            .unwrap();
        assert_eq!(
            handed.message_history, row.message_history,
            "history is not rewritten"
        );
        if let Some(dir) = cache_dir("r-hand-off-test") {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}
