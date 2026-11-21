//! Reading a zip archive without a shell (AGE-1785): `list_archive` names
//! its members and `extract_archive` unpacks it into a fresh folder in the
//! workspace, so a worker handed a `.zip` can read what is inside with the
//! document readers.
//!
//! Extracted files are inputs to read, not results: the default destination
//! and a relative `dest` are taken from the workspace root, never from the
//! output root. Beside the archive would fail in a read-only `inputs/`
//! folder, and under `outputs/` they would be handed in as deliverables.
//!
//! Extraction is read-only towards everything that exists: it writes only
//! into a directory that did not exist before the call, never overwrites a
//! file, never creates or follows a symlink, and refuses an archive past
//! [`ArchiveLimits`] (member count, total size, compression ratio) with
//! nothing left behind. Members are unpacked into a hidden staging
//! directory beside the destination, which is renamed into place only once
//! every member is written; on any failure the staging directory, and any
//! parent directory the call created, is removed. Size and ratio are
//! checked on the header first and again on the bytes actually written,
//! so a header that lies about a member's size does not get past them.
//! Nested archives are extracted as files, not recursed into.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use zip::CompressionMethod;

use crate::models::write_approval_store::PendingWriteApprovals;
use crate::services::filesystem_service::FileSystemService;
use crate::settings::models::execution_settings::ApprovalMode;

use super::path_utils::{approve_output_write, resolve_fresh_workspace_dir};

#[derive(Debug, thiserror::Error)]
pub enum ArchiveToolError {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    OperationError(#[from] anyhow::Error),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

fn refused(message: impl Into<String>) -> ArchiveToolError {
    ArchiveToolError::Refused(message.into())
}

/// What an archive may hold before extraction refuses it.
#[derive(Clone, Copy, Debug)]
pub struct ArchiveLimits {
    /// Most members (files and directories) an archive may have.
    pub max_members: usize,
    /// Most bytes all members together may unpack to.
    pub max_total_bytes: u64,
    /// Highest uncompressed:compressed ratio a member may have ...
    pub max_ratio: u64,
    /// ... once it unpacks to more than this many bytes: a small text file
    /// of repeated characters compresses past any sane ratio.
    pub ratio_floor_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_members: 1_000,
            max_total_bytes: 200 * 1024 * 1024,
            max_ratio: 100,
            ratio_floor_bytes: 1024 * 1024,
        }
    }
}

impl ArchiveLimits {
    /// Bytes a member stored in `compressed` bytes may unpack to.
    fn member_cap(&self, compressed: u64) -> u64 {
        compressed
            .saturating_mul(self.max_ratio)
            .max(self.ratio_floor_bytes)
    }
}

#[derive(Debug, Serialize)]
pub struct ArchiveMember {
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    pub is_dir: bool,
}

#[derive(Deserialize, Serialize)]
pub struct ListArchiveArgs {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ListArchiveOutput {
    pub path: String,
    pub member_count: usize,
    /// Sum of the members' uncompressed sizes, as their headers declare.
    pub total_size: u64,
    pub members: Vec<ArchiveMember>,
    /// Whether `members` stops short of `member_count`.
    pub truncated: bool,
}

#[derive(Deserialize, Serialize)]
pub struct ExtractArchiveArgs {
    pub path: String,
    /// A directory that does not exist yet, relative to the workspace root
    /// or absolute inside it. Defaults to the archive's name without its
    /// extension, at the workspace root.
    #[serde(default)]
    pub dest: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExtractArchiveOutput {
    pub archive: String,
    pub dest: String,
    /// The extracted files, as paths relative to the workspace root.
    pub files: Vec<String>,
    pub file_count: usize,
    pub bytes_written: u64,
}

/// Open `path` as a zip archive.
fn open_archive(path: &Path) -> Result<zip::ZipArchive<File>, ArchiveToolError> {
    let file = File::open(path)?;
    zip::ZipArchive::new(file).map_err(|e| {
        refused(format!(
            "'{}' is not a readable zip archive: {e}",
            path.display()
        ))
    })
}

/// A member's name as a relative path of plain components, or why it is
/// refused: absolute, a drive or root prefix, a `..` anywhere, a NUL byte,
/// or a name the zip crate's own `enclosed_name` will not vouch for.
fn safe_member_path(name: &str, enclosed: Option<PathBuf>) -> Result<PathBuf, String> {
    let unsafe_name = |why: &str| format!("member '{name}' {why}; refusing the archive");
    if name.contains('\0') {
        return Err(unsafe_name("has a NUL byte in its name"));
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(unsafe_name("is an absolute path"));
    }
    let segments: Vec<&str> = name.split(['/', '\\']).collect();
    if segments.first().is_some_and(|first| first.contains(':')) {
        return Err(unsafe_name("has a drive prefix"));
    }
    if segments.contains(&"..") {
        return Err(unsafe_name("climbs out of its folder with '..'"));
    }
    if enclosed.is_none() {
        return Err(unsafe_name("does not stay inside the archive's folder"));
    }
    let path: PathBuf = segments
        .into_iter()
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(unsafe_name("has no usable name"));
    }
    Ok(path)
}

/// One member as the pre-scan accepted it.
struct PlannedMember {
    index: usize,
    path: PathBuf,
    is_dir: bool,
    declared_size: u64,
    compressed_size: u64,
}

/// Check every member's header against `limits` before anything is written.
fn plan_extraction(
    archive: &mut zip::ZipArchive<File>,
    limits: &ArchiveLimits,
) -> Result<Vec<PlannedMember>, ArchiveToolError> {
    if archive.len() > limits.max_members {
        return Err(refused(format!(
            "the archive has {} members, more than the {} allowed",
            archive.len(),
            limits.max_members
        )));
    }
    let mut planned = Vec::with_capacity(archive.len());
    let mut seen = std::collections::HashSet::new();
    let mut declared_total: u64 = 0;
    for index in 0..archive.len() {
        let member = archive
            .by_index_raw(index)
            .map_err(|e| refused(format!("could not read member {index}: {e}")))?;
        let name = member
            .name()
            .map_err(|e| refused(format!("member {index}: {e}")))?;
        let name = name.into_owned();
        let path = safe_member_path(&name, member.enclosed_name()).map_err(refused)?;
        if member.is_symlink() {
            return Err(refused(format!(
                "member '{name}' is a symlink; symlinks are not extracted, refusing the archive"
            )));
        }
        if member.encrypted() {
            return Err(refused(format!("member '{name}' is encrypted")));
        }
        let is_dir = member.is_dir();
        if !is_dir
            && !matches!(
                member.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
        {
            return Err(refused(format!(
                "member '{name}' uses {:?} compression; only stored and deflated members are extracted",
                member.compression()
            )));
        }
        if !seen.insert(path.clone()) {
            return Err(refused(format!(
                "member '{name}' appears twice; refusing rather than overwrite it"
            )));
        }
        let (declared_size, compressed_size) = (member.size(), member.compressed_size());
        if declared_size > limits.member_cap(compressed_size) {
            return Err(refused(format!(
                "member '{name}' unpacks {compressed_size} bytes to {declared_size}, past the \
                 {}:1 compression ratio allowed (a zip bomb?)",
                limits.max_ratio
            )));
        }
        declared_total = declared_total.saturating_add(declared_size);
        if declared_total > limits.max_total_bytes {
            return Err(refused(format!(
                "the archive unpacks to more than the {} bytes allowed",
                limits.max_total_bytes
            )));
        }
        planned.push(PlannedMember {
            index,
            path,
            is_dir,
            declared_size,
            compressed_size,
        });
    }
    Ok(planned)
}

/// Unpack `planned` into the empty directory `staging`, counting the bytes
/// actually written against `limits`. Returns the written files (relative to
/// `staging`) and the total byte count.
fn write_members(
    archive: &mut zip::ZipArchive<File>,
    planned: &[PlannedMember],
    staging: &Path,
    limits: &ArchiveLimits,
) -> Result<(Vec<PathBuf>, u64), ArchiveToolError> {
    let mut files = Vec::new();
    let mut total: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    for member in planned {
        let out = staging.join(&member.path);
        if member.is_dir {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // `create_new`: never an existing entry, a symlink included.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)?;
        let mut reader = archive
            .by_index(member.index)
            .map_err(|e| refused(format!("could not read '{}': {e}", member.path.display())))?;
        let cap = limits.member_cap(member.compressed_size);
        copy_counted(
            &mut reader,
            &mut file,
            member,
            cap,
            limits,
            &mut total,
            &mut buf,
        )?;
        file.flush()?;
        files.push(member.path.clone());
    }
    Ok((files, total))
}

/// Copy `reader` into `file`, counting the bytes actually read against the
/// member's declared size, its ratio `cap` and the archive total in `limits`.
fn copy_counted<R: Read, W: Write>(
    reader: &mut R,
    file: &mut W,
    member: &PlannedMember,
    cap: u64,
    limits: &ArchiveLimits,
    total: &mut u64,
    buf: &mut [u8],
) -> Result<(), ArchiveToolError> {
    let mut written: u64 = 0;
    loop {
        let n = reader
            .read(buf)
            .map_err(|e| refused(format!("could not unpack '{}': {e}", member.path.display())))?;
        if n == 0 {
            return Ok(());
        }
        written += n as u64;
        *total += n as u64;
        if written > member.declared_size {
            return Err(refused(format!(
                "member '{}' unpacks to more than the {} bytes its header declares (a zip bomb?)",
                member.path.display(),
                member.declared_size
            )));
        }
        if written > cap {
            return Err(refused(format!(
                "member '{}' unpacks past the {}:1 compression ratio allowed (a zip bomb?)",
                member.path.display(),
                limits.max_ratio
            )));
        }
        if *total > limits.max_total_bytes {
            return Err(refused(format!(
                "the archive unpacks to more than the {} bytes allowed",
                limits.max_total_bytes
            )));
        }
        file.write_all(&buf[..n])?;
    }
}

/// The topmost ancestor of `path` (itself included) that does not exist
/// yet: what to remove if the call fails after creating `path`'s parents.
fn first_missing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut missing = None;
    let mut current = Some(path);
    while let Some(p) = current {
        if std::fs::symlink_metadata(p).is_ok() {
            break;
        }
        missing = Some(p.to_path_buf());
        current = p.parent();
    }
    missing
}

/// Extract `archive_path` into `dest`, which must not exist, under
/// `limits`. Blocking. On failure nothing is left: not the staging
/// directory, not `dest`, not a parent directory this call created.
pub fn extract_zip(
    archive_path: &Path,
    dest: &Path,
    limits: &ArchiveLimits,
) -> Result<(Vec<PathBuf>, u64), ArchiveToolError> {
    let mut archive = open_archive(archive_path)?;
    let planned = plan_extraction(&mut archive, limits)?;
    if std::fs::symlink_metadata(dest).is_ok() {
        return Err(refused(format!(
            "'{}' already exists; extract_archive only writes into a new folder",
            dest.display()
        )));
    }
    let parent = dest
        .parent()
        .ok_or_else(|| refused(format!("'{}' has no parent folder", dest.display())))?;
    let created_parent = first_missing_ancestor(parent);
    let cleanup_parent = || {
        if let Some(created) = &created_parent
            && let Err(e) = std::fs::remove_dir_all(created)
        {
            tracing::warn!(error = ?e, path = %created.display(), "Failed to remove a folder extract_archive created");
        }
    };
    std::fs::create_dir_all(parent)?;
    let dest_name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Dropped (and so removed) on every early return below.
    let staging = match tempfile::Builder::new()
        .prefix(&format!(".{dest_name}.extracting-"))
        .tempdir_in(parent)
    {
        Ok(staging) => staging,
        Err(e) => {
            cleanup_parent();
            return Err(e.into());
        }
    };
    let result =
        write_members(&mut archive, &planned, staging.path(), limits).and_then(|written| {
            // Checked again: rename(2) would replace an empty directory
            // created there since the first check.
            if std::fs::symlink_metadata(dest).is_ok() {
                return Err(refused(format!(
                    "'{}' already exists; extract_archive only writes into a new folder",
                    dest.display()
                )));
            }
            std::fs::rename(staging.path(), dest)?;
            Ok(written)
        });
    match result {
        Ok(written) => {
            // The staging path is gone (renamed); nothing for its drop to do.
            let _ = staging.keep();
            Ok(written)
        }
        Err(e) => {
            drop(staging);
            cleanup_parent();
            Err(e)
        }
    }
}

/// `list_archive`: the members of a zip archive, without extracting it.
#[derive(Clone)]
pub struct ListArchiveTool {
    service: Arc<FileSystemService>,
}

impl ListArchiveTool {
    /// Members listed per call; `member_count` still counts them all.
    const MAX_LISTED: usize = 1_000;

    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for ListArchiveTool {
    const NAME: &'static str = "list_archive";
    type Error = ArchiveToolError;
    type Args = ListArchiveArgs;
    type Output = ListArchiveOutput;

    fn description(&self) -> String {
        "List the members of a zip archive (.zip) without extracting it: each member's name, \
         uncompressed size, compressed size and whether it is a directory.\n\
         \n\
         Use this for .zip files — do NOT use read_file (binary garbage). To read a member, \
         extract the archive with extract_archive, then read the extracted file with the \
         matching reader (read_docx, read_excel, read_pptx, pdf_extract_text, read_file).\n\
         \n\
         Example: {\"path\": \"attachments/data.zip\"}"
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the .zip file, relative to workspace root or absolute within workspace"
                }
            },
            "required": ["path"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_path(&args.path).await?;
        let path = canonical.clone();
        let (member_count, total_size, members) = tokio::task::spawn_blocking(move || {
            let mut archive = open_archive(&path)?;
            let mut members = Vec::new();
            let mut total_size: u64 = 0;
            for index in 0..archive.len() {
                let member = archive
                    .by_index_raw(index)
                    .map_err(|e| refused(format!("could not read member {index}: {e}")))?;
                total_size = total_size.saturating_add(member.size());
                if members.len() < Self::MAX_LISTED {
                    members.push(ArchiveMember {
                        name: member.name().map(|n| n.into_owned()).unwrap_or_else(|_| {
                            String::from_utf8_lossy(member.name_raw()).into_owned()
                        }),
                        size: member.size(),
                        compressed_size: member.compressed_size(),
                        is_dir: member.is_dir(),
                    });
                }
            }
            Ok::<_, ArchiveToolError>((archive.len(), total_size, members))
        })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list '{}': {e}", args.path))??;
        Ok(ListArchiveOutput {
            path: canonical.display().to_string(),
            member_count,
            total_size,
            truncated: members.len() < member_count,
            members,
        })
    }
}

/// `extract_archive`: unpack a zip archive into a new folder in the
/// workspace, under [`ArchiveLimits::default`].
#[derive(Clone)]
pub struct ExtractArchiveTool {
    service: Arc<FileSystemService>,
    write_approval: Option<(ApprovalMode, PendingWriteApprovals)>,
    /// The output root (AGE-1747), named in the description only:
    /// extracted files never go under it unless `dest` says so.
    output_root: Option<PathBuf>,
    limits: ArchiveLimits,
}

impl ExtractArchiveTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self {
            service,
            write_approval: None,
            output_root: None,
            limits: ArchiveLimits::default(),
        }
    }

    /// Ask for write approval under `mode` before extracting, like every
    /// other file write. The agent factory always sets this.
    pub fn with_write_approval(
        mut self,
        mode: ApprovalMode,
        pending: PendingWriteApprovals,
    ) -> Self {
        self.write_approval = Some((mode, pending));
        self
    }

    /// Tell the model that extracted files do not go under `output_root`
    /// (AGE-1747): a relative `dest` is still taken from the workspace root.
    pub fn with_output_root(mut self, output_root: Option<PathBuf>) -> Self {
        self.output_root = output_root;
        self
    }

    #[cfg(test)]
    fn with_limits(mut self, limits: ArchiveLimits) -> Self {
        self.limits = limits;
        self
    }
}

impl Tool for ExtractArchiveTool {
    const NAME: &'static str = "extract_archive";
    type Error = ArchiveToolError;
    type Args = ExtractArchiveArgs;
    type Output = ExtractArchiveOutput;

    fn description(&self) -> String {
        let limits = self.limits;
        let not_outputs = self
            .output_root
            .as_deref()
            .map_or_else(String::new, |root| {
                format!(
                    "\n\nExtracted files are inputs to read, not results: they are not placed \
                 under `{}/`.",
                    root.display()
                )
            });
        format!(
            "Extract a zip archive (.zip) into a NEW folder in the workspace and return the \
             extracted files. Then read each file with the matching reader (read_docx, \
             read_excel, read_pptx, pdf_extract_text, read_file).\n\
             \n\
             The destination must not exist yet; by default it is a new folder at the \
             workspace root named after the archive without .zip (inputs/data.zip -> data/). \
             A relative dest is taken from the workspace root too. Nothing is overwritten. \
             Refused, with nothing written: more than {} members, more than {} MB unpacked, \
             a member compressed more than {}:1, a member whose path is absolute or uses '..', \
             a symlink member, an encrypted member. Archives inside the archive are extracted \
             as files, not unpacked.\n\
             \n\
             Examples:\n\
             - Into data/ at the workspace root: {{\"path\": \"attachments/data.zip\"}}\n\
             - Into a chosen folder: {{\"path\": \"data.zip\", \"dest\": \"data_unzipped\"}}\
             {not_outputs}",
            limits.max_members,
            limits.max_total_bytes / (1024 * 1024),
            limits.max_ratio,
        )
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the .zip file, relative to workspace root or absolute within workspace"
                },
                "dest": {
                    "type": "string",
                    "description": "Folder to extract into, relative to the workspace root or absolute within it; must not exist yet. Defaults to the archive's name without .zip, at the workspace root."
                }
            },
            "required": ["path"]
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let archive = self.service.resolve_path(&args.path).await?;
        let root = self.service.workspace_root().to_path_buf();
        let dest = match args.dest.as_deref() {
            // Never under the output root: extracted files are inputs.
            Some(dest) => resolve_fresh_workspace_dir(&root, dest),
            None => {
                let stem = archive
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        refused(format!("'{}' has no name to extract under", args.path))
                    })?;
                // At the root, not beside the archive: its folder may be
                // read-only (hive's `inputs/`).
                resolve_fresh_workspace_dir(&root, &root.join(stem).to_string_lossy())
            }
        }
        .map_err(|e| {
            refused(format!(
                "{e}. Give `dest`: a folder inside the workspace that does not exist yet"
            ))
        })?;

        if !approve_output_write(
            self.write_approval.as_ref(),
            &dest,
            format!(
                "Extract the zip archive {} into a new folder",
                archive.display()
            ),
        )
        .await
        .map_err(refused)?
        {
            return Err(refused("Write denied by user"));
        }

        let limits = self.limits;
        let (archive_path, dest_path) = (archive.clone(), dest.clone());
        let (files, bytes_written) =
            tokio::task::spawn_blocking(move || extract_zip(&archive_path, &dest_path, &limits))
                .await
                .map_err(|e| anyhow::anyhow!("Failed to extract '{}': {e}", args.path))??;

        let files: Vec<String> = files
            .iter()
            .map(|file| {
                let full = dest.join(file);
                full.strip_prefix(&root)
                    .unwrap_or(&full)
                    .display()
                    .to_string()
            })
            .collect();
        Ok(ExtractArchiveOutput {
            archive: archive.display().to_string(),
            dest: dest.display().to_string(),
            file_count: files.len(),
            files,
            bytes_written,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use rig_agent::tool::{Tool, ToolContext};
    use zip::write::SimpleFileOptions;

    use super::*;

    async fn workspace() -> (tempfile::TempDir, PathBuf, Arc<FileSystemService>) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let service = Arc::new(
            FileSystemService::new(root.to_str().unwrap())
                .await
                .unwrap(),
        );
        (tmp, root, service)
    }

    fn deflated() -> SimpleFileOptions {
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
    }

    /// A zip of `(name, bytes)` members, deflated.
    fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in members {
            writer.start_file(*name, deflated()).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// Everything under `root`, as sorted relative paths.
    fn tree(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                out.push(path.strip_prefix(root).unwrap().display().to_string());
                if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                    stack.push(path);
                }
            }
        }
        out.sort();
        out
    }

    async fn extract(
        service: &Arc<FileSystemService>,
        path: &str,
        dest: Option<&str>,
        limits: ArchiveLimits,
    ) -> Result<ExtractArchiveOutput, ArchiveToolError> {
        ExtractArchiveTool::new(service.clone())
            .with_limits(limits)
            .call(
                &mut ToolContext::new(),
                ExtractArchiveArgs {
                    path: path.to_string(),
                    dest: dest.map(str::to_string),
                },
            )
            .await
    }

    /// Extraction of `zip` is refused with a message containing `expect`,
    /// and the workspace holds exactly the archive afterwards.
    async fn assert_refused(zip: Vec<u8>, limits: ArchiveLimits, expect: &str) {
        let (_tmp, root, service) = workspace().await;
        std::fs::write(root.join("in.zip"), zip).unwrap();
        let err = extract(&service, "in.zip", None, limits)
            .await
            .expect_err("the archive is refused");
        assert!(err.to_string().contains(expect), "{err}");
        assert_eq!(tree(&root), vec!["in.zip"], "nothing is left behind");
    }

    #[tokio::test]
    async fn list_archive_names_every_member() {
        let (_tmp, root, service) = workspace().await;
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer.add_directory("docs/", deflated()).unwrap();
        writer.start_file("docs/a.txt", deflated()).unwrap();
        writer.write_all(&[b'a'; 4000]).unwrap();
        std::fs::write(root.join("x.zip"), writer.finish().unwrap().into_inner()).unwrap();

        let out = ListArchiveTool::new(service)
            .call(
                &mut ToolContext::new(),
                ListArchiveArgs {
                    path: "x.zip".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(out.member_count, 2);
        assert!(!out.truncated);
        assert!(out.members[0].is_dir && out.members[0].name == "docs/");
        let file = &out.members[1];
        assert_eq!(
            (file.name.as_str(), file.size, file.is_dir),
            ("docs/a.txt", 4000, false)
        );
        assert!(file.compressed_size < file.size, "{file:?}");
        assert_eq!(out.total_size, 4000);
    }

    #[tokio::test]
    async fn a_non_zip_is_refused_by_both_tools() {
        let (_tmp, root, service) = workspace().await;
        std::fs::write(root.join("x.zip"), b"not a zip").unwrap();
        let err = ListArchiveTool::new(service.clone())
            .call(
                &mut ToolContext::new(),
                ListArchiveArgs {
                    path: "x.zip".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not a readable zip archive"),
            "{err}"
        );
        assert!(
            extract(&service, "x.zip", None, ArchiveLimits::default())
                .await
                .is_err()
        );
        assert_eq!(tree(&root), vec!["x.zip"]);
    }

    #[tokio::test]
    async fn extracts_at_the_workspace_root_by_default_and_into_a_given_dest() {
        let (_tmp, root, service) = workspace().await;
        std::fs::create_dir(root.join("in")).unwrap();
        let zip = zip_of(&[
            ("a.txt", b"alpha"),
            ("sub/b.txt", b"beta"),
            ("./c.txt", b"c"),
        ]);
        std::fs::write(root.join("in/pack.zip"), &zip).unwrap();

        let out = extract(&service, "in/pack.zip", None, ArchiveLimits::default())
            .await
            .unwrap();
        assert_eq!(out.dest, root.join("pack").display().to_string());
        assert_eq!(
            out.files,
            vec!["pack/a.txt", "pack/sub/b.txt", "pack/c.txt"]
        );
        assert_eq!((out.file_count, out.bytes_written), (3, 10));
        assert_eq!(std::fs::read(root.join("pack/sub/b.txt")).unwrap(), b"beta");
        assert!(!root.join("in/pack").exists(), "not beside the archive");

        let out = extract(
            &service,
            "in/pack.zip",
            Some("new/deeper"),
            ArchiveLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.files[0], "new/deeper/a.txt");
        assert!(root.join("new/deeper/c.txt").is_file());
        // No staging directory left anywhere.
        assert!(
            tree(&root).iter().all(|p| !p.contains(".extracting-")),
            "{:?}",
            tree(&root)
        );
    }

    /// hive's layout: attachments in `inputs/`, results in `outputs/` (the
    /// output root). Extracted files are inputs, so none land in `outputs/`.
    async fn hive_layout() -> (tempfile::TempDir, PathBuf, ExtractArchiveTool) {
        let (tmp, root, service) = workspace().await;
        std::fs::create_dir_all(root.join("inputs")).unwrap();
        std::fs::create_dir_all(root.join("outputs")).unwrap();
        std::fs::write(
            root.join("inputs/pack.zip"),
            zip_of(&[("a.txt", b"a"), ("sub/b.txt", b"b")]),
        )
        .unwrap();
        let tool =
            ExtractArchiveTool::new(service).with_output_root(Some(PathBuf::from("outputs")));
        (tmp, root, tool)
    }

    async fn call(tool: &ExtractArchiveTool, dest: Option<&str>) -> ExtractArchiveOutput {
        tool.call(
            &mut ToolContext::new(),
            ExtractArchiveArgs {
                path: "inputs/pack.zip".into(),
                dest: dest.map(str::to_string),
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_default_dest_ignores_the_output_root() {
        let (_tmp, root, tool) = hive_layout().await;
        let out = call(&tool, None).await;
        assert_eq!(out.dest, root.join("pack").display().to_string());
        assert_eq!(out.files, vec!["pack/a.txt", "pack/sub/b.txt"]);
        assert_eq!(std::fs::read(root.join("pack/sub/b.txt")).unwrap(), b"b");
        assert_eq!(std::fs::read_dir(root.join("outputs")).unwrap().count(), 0);
        let description = tool.description();
        assert!(
            description.contains("at the workspace root"),
            "{description}"
        );
        assert!(
            description.contains("not placed under `outputs/`"),
            "{description}"
        );
    }

    #[tokio::test]
    async fn a_relative_dest_is_taken_from_the_workspace_root() {
        let (_tmp, root, tool) = hive_layout().await;
        let out = call(&tool, Some("unpacked")).await;
        assert_eq!(out.files, vec!["unpacked/a.txt", "unpacked/sub/b.txt"]);
        assert!(root.join("unpacked/a.txt").is_file());
        assert_eq!(std::fs::read_dir(root.join("outputs")).unwrap().count(), 0);
    }

    /// hive's `inputs/` is root-owned 0555 with 0444 files: the default
    /// dest must not need to write there.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_archive_in_a_read_only_folder_still_extracts() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, root, tool) = hive_layout().await;
        let inputs = root.join("inputs");
        let mode = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        mode(&inputs.join("pack.zip"), 0o444);
        mode(&inputs, 0o555);
        if std::fs::write(inputs.join("probe"), b"").is_ok() {
            // Running as root: permissions do not bind, nothing to test.
            mode(&inputs, 0o755);
            return;
        }
        let out = call(&tool, None).await;
        mode(&inputs, 0o755);
        assert_eq!(out.files, vec!["pack/a.txt", "pack/sub/b.txt"]);
        assert!(root.join("pack/sub/b.txt").is_file());
        assert_eq!(tree(&inputs), vec!["pack.zip"]);
    }

    #[tokio::test]
    async fn a_dest_that_exists_is_refused_and_left_alone() {
        let (_tmp, root, service) = workspace().await;
        std::fs::write(root.join("in.zip"), zip_of(&[("a.txt", b"new")])).unwrap();
        std::fs::create_dir(root.join("in")).unwrap();
        std::fs::write(root.join("in/a.txt"), b"keep").unwrap();
        std::fs::write(root.join("file"), b"keep").unwrap();

        for dest in [None, Some("in"), Some("file"), Some(".")] {
            let err = extract(&service, "in.zip", dest, ArchiveLimits::default())
                .await
                .expect_err("an existing dest is refused");
            assert!(
                err.to_string().contains("does not exist yet"),
                "{dest:?}: {err}"
            );
        }
        assert_eq!(std::fs::read(root.join("in/a.txt")).unwrap(), b"keep");
        assert_eq!(tree(&root), vec!["file", "in", "in.zip", "in/a.txt"]);
    }

    #[tokio::test]
    async fn a_dest_outside_the_workspace_is_refused() {
        let (_tmp, root, service) = workspace().await;
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.join("in.zip"), zip_of(&[("a.txt", b"a")])).unwrap();
        let away = outside.path().join("x");
        for dest in ["../escape", "/etc/chatty-x", "~/x", away.to_str().unwrap()] {
            let err = extract(&service, "in.zip", Some(dest), ArchiveLimits::default())
                .await
                .expect_err("outside is refused");
            let message = err.to_string();
            assert!(
                message.contains("outside the workspace") || message.contains("'~' is not allowed"),
                "{dest}: {message}"
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
            let err = extract(&service, "in.zip", Some("link/x"), ArchiveLimits::default())
                .await
                .expect_err("a symlinked parent out of the workspace is refused");
            assert!(err.to_string().contains("outside the workspace"), "{err}");
        }
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_parent_climbing_member_is_refused() {
        assert_refused(
            zip_of(&[("ok.txt", b"ok"), ("../evil", b"x")]),
            ArchiveLimits::default(),
            "'..'",
        )
        .await;
        assert_refused(
            zip_of(&[("a/../../evil", b"x")]),
            ArchiveLimits::default(),
            "'..'",
        )
        .await;
        assert_refused(
            zip_of(&[("a\\..\\evil", b"x")]),
            ArchiveLimits::default(),
            "'..'",
        )
        .await;
    }

    #[tokio::test]
    async fn an_absolute_member_is_refused() {
        assert_refused(
            zip_of(&[("ok.txt", b"ok"), ("/etc/x", b"x")]),
            ArchiveLimits::default(),
            "absolute",
        )
        .await;
        assert_refused(
            zip_of(&[("C:/x", b"x")]),
            ArchiveLimits::default(),
            "drive prefix",
        )
        .await;
        assert_refused(
            zip_of(&[("\\x", b"x")]),
            ArchiveLimits::default(),
            "absolute",
        )
        .await;
    }

    #[tokio::test]
    async fn a_symlink_member_is_refused() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file("ok.txt", deflated()).unwrap();
        writer.write_all(b"ok").unwrap();
        writer
            .add_symlink("link", "/etc/passwd", deflated())
            .unwrap();
        assert_refused(
            writer.finish().unwrap().into_inner(),
            ArchiveLimits::default(),
            "symlink",
        )
        .await;
    }

    /// Two members with one name: the zip reader keeps one entry per name,
    /// so one file is written, once, with `create_new`; never one over the
    /// other. (The pre-scan's own duplicate check is the backstop for a
    /// reader that did list both.)
    #[tokio::test]
    async fn a_duplicate_member_name_writes_one_file_once() {
        // The zip writer refuses duplicate names, so rename one in the bytes.
        let mut zip = zip_of(&[("a.txt", b"one"), ("b.txt", b"two")]);
        for i in 0..zip.len() - 5 {
            if &zip[i..i + 5] == b"b.txt" {
                zip[i] = b'a';
            }
        }
        let (_tmp, root, service) = workspace().await;
        std::fs::write(root.join("in.zip"), zip).unwrap();
        let out = extract(&service, "in.zip", None, ArchiveLimits::default())
            .await
            .unwrap();
        assert_eq!(out.files, vec!["in/a.txt"]);
        assert_eq!(out.bytes_written, 3);
        assert_eq!(tree(&root), vec!["in", "in.zip", "in/a.txt"]);
    }

    #[tokio::test]
    async fn a_high_ratio_member_is_refused_on_its_header() {
        // 10 MB of zeros deflates to about 10 kB: 1000:1.
        let zeros = vec![0u8; 10 * 1024 * 1024];
        assert_refused(
            zip_of(&[("ok.txt", b"ok"), ("bomb.bin", &zeros)]),
            ArchiveLimits::default(),
            "compression ratio",
        )
        .await;
    }

    #[tokio::test]
    async fn more_than_the_total_cap_is_refused() {
        let limits = ArchiveLimits {
            max_total_bytes: 1000,
            ..ArchiveLimits::default()
        };
        let part: Vec<u8> = (0..600u32).map(|i| (i * 7919 % 251) as u8).collect();
        assert_refused(
            zip_of(&[("a.bin", &part), ("b.bin", &part)]),
            limits,
            "bytes allowed",
        )
        .await;
    }

    #[tokio::test]
    async fn more_members_than_allowed_is_refused() {
        let limits = ArchiveLimits {
            max_members: 2,
            ..ArchiveLimits::default()
        };
        assert_refused(
            zip_of(&[("a", b"a"), ("b", b"b"), ("c", b"c")]),
            limits,
            "members",
        )
        .await;
    }

    /// A zip whose headers say `bomb.bin` unpacks to 1 kB while it really
    /// unpacks to 10 MB: the bytes written, not the header, stop it.
    #[tokio::test]
    async fn a_member_larger_than_its_header_says_is_stopped_while_writing() {
        let zeros = vec![0u8; 10 * 1024 * 1024];
        let mut zip = zip_of(&[("ok.txt", b"ok"), ("bomb.bin", &zeros)]);
        let lie = 1024u32.to_le_bytes();
        let mut patched = 0;
        for i in 0..zip.len() - 4 {
            // Local header: uncompressed size at +22; central: at +24.
            let field = match &zip[i..i + 4] {
                [0x50, 0x4b, 0x03, 0x04] => i + 22,
                [0x50, 0x4b, 0x01, 0x02] => i + 24,
                _ => continue,
            };
            let name_at = if zip[i + 2] == 0x03 { i + 30 } else { i + 46 };
            if zip[name_at..].starts_with(b"bomb.bin") {
                zip[field..field + 4].copy_from_slice(&lie);
                patched += 1;
            }
        }
        assert_eq!(patched, 2, "both headers patched");
        // The header now passes the ratio check; writing does not.
        let (_tmp, root, service) = workspace().await;
        std::fs::write(root.join("in.zip"), &zip).unwrap();
        let err = extract(
            &service,
            "in.zip",
            Some("deep/out"),
            ArchiveLimits::default(),
        )
        .await
        .expect_err("the lying member is stopped");
        let message = err.to_string();
        assert!(
            message.contains("bytes its header declares") || message.contains("unpack"),
            "{message}"
        );
        assert_eq!(
            tree(&root),
            vec!["in.zip"],
            "no partial output, not even `deep/`"
        );
    }

    /// chatty's own counters, with the zip crate's size check out of the way:
    /// a stub reader whose bytes outgrow the declared size and the ratio cap.
    fn counted(declared_size: u64, cap: u64, bytes: usize) -> Result<Vec<u8>, String> {
        let member = PlannedMember {
            index: 0,
            path: PathBuf::from("m.bin"),
            is_dir: false,
            declared_size,
            compressed_size: 1,
        };
        let mut out = Vec::new();
        let mut total = 0;
        let mut buf = [0u8; 16];
        copy_counted(
            &mut std::io::repeat(7).take(bytes as u64),
            &mut out,
            &member,
            cap,
            &ArchiveLimits::default(),
            &mut total,
            &mut buf,
        )
        .map(|()| out)
        .map_err(|e| e.to_string())
    }

    #[test]
    fn the_counter_stops_bytes_past_the_declared_size() {
        assert_eq!(counted(40, u64::MAX, 40).unwrap().len(), 40);
        let err = counted(40, u64::MAX, 41).unwrap_err();
        assert!(err.contains("bytes its header declares"), "{err}");
    }

    #[test]
    fn the_counter_stops_bytes_past_the_ratio_cap() {
        assert_eq!(counted(u64::MAX, 40, 40).unwrap().len(), 40);
        let err = counted(u64::MAX, 40, 41).unwrap_err();
        assert!(err.contains("compression ratio"), "{err}");
    }

    #[test]
    fn member_names_are_sanitised() {
        let ok = |name: &str| safe_member_path(name, Some(PathBuf::from(name)));
        assert_eq!(ok("a/./b.txt").unwrap(), PathBuf::from("a/b.txt"));
        assert_eq!(ok("dir/").unwrap(), PathBuf::from("dir"));
        for bad in [
            "../x",
            "a/../../x",
            "/abs",
            "\\abs",
            "C:x",
            "c:\\x",
            "a\\..\\x",
            ".",
            "",
            "a\0b",
        ] {
            assert!(ok(bad).is_err(), "{bad:?} is refused");
        }
        assert!(
            safe_member_path("fine", None).is_err(),
            "enclosed_name must agree"
        );
    }

    /// AGE-1785's done-when: a zip holding a PDF and a docx lists both,
    /// extracts both into a fresh folder, and the document readers read
    /// what was extracted.
    #[cfg(all(feature = "pdf", feature = "docx"))]
    #[tokio::test]
    async fn a_zip_with_a_pdf_and_a_docx_is_listed_extracted_and_read() {
        use crate::tools::docx_tool::{ReadDocxTool, WriteDocxTool};
        use crate::tools::pdf_extract_text_tool::{PdfExtractTextArgs, PdfExtractTextTool};

        let (_tmp, root, service) = workspace().await;
        // Make the docx with write_docx, then move both into a zip.
        let made = tempfile::tempdir().unwrap();
        let maker = Arc::new(
            FileSystemService::new(made.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        WriteDocxTool::new(maker)
            .call(
                &mut ToolContext::new(),
                serde_json::from_value(serde_json::json!({
                    "path": "memo.docx",
                    "content": "# Memo\n\nThe answer is forty-two."
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        let docx = std::fs::read(made.path().join("memo.docx")).unwrap();
        let pdf = one_page_pdf("Revenue grew 12 percent");
        std::fs::write(
            root.join("task.zip"),
            zip_of(&[("files/report.pdf", &pdf), ("files/memo.docx", &docx)]),
        )
        .unwrap();

        let listed = ListArchiveTool::new(service.clone())
            .call(
                &mut ToolContext::new(),
                ListArchiveArgs {
                    path: "task.zip".into(),
                },
            )
            .await
            .unwrap();
        let names: Vec<&str> = listed.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["files/report.pdf", "files/memo.docx"]);
        assert_eq!(listed.members[0].size, pdf.len() as u64);
        assert_eq!(listed.members[1].size, docx.len() as u64);

        assert!(!root.join("task").exists());
        let out = extract(&service, "task.zip", None, ArchiveLimits::default())
            .await
            .unwrap();
        assert_eq!(
            out.files,
            vec!["task/files/report.pdf", "task/files/memo.docx"]
        );

        let text = PdfExtractTextTool::new(service.clone())
            .call(
                &mut ToolContext::new(),
                PdfExtractTextArgs {
                    path: out.files[0].clone(),
                    pages: None,
                },
            )
            .await
            .unwrap();
        assert!(
            serde_json::to_string(&text)
                .unwrap()
                .contains("Revenue grew 12 percent"),
            "{:?}",
            serde_json::to_string(&text)
        );
        let memo = ReadDocxTool::new(service)
            .call(
                &mut ToolContext::new(),
                serde_json::from_value(serde_json::json!({ "path": out.files[1] })).unwrap(),
            )
            .await
            .unwrap();
        assert!(
            memo.text.contains("The answer is forty-two."),
            "{}",
            memo.text
        );
    }

    /// A one-page PDF with correct xref offsets and `text` on the page.
    #[cfg(feature = "pdf")]
    fn one_page_pdf(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 12 Tf 72 700 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }
}
