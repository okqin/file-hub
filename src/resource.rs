//! Storage-root-backed resource listing.

use std::{
    cmp::Ordering,
    fmt::Write as FmtWrite,
    io::{Cursor, Write},
    path::PathBuf,
};

use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions as CapOpenOptions},
};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
    task,
};
use tracing::warn;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::config::AppConfig;

const MAX_SEARCH_QUERY_BYTES: usize = 256;
const MAX_RESOURCE_NAME_BYTES: usize = 255;
pub(crate) const MAX_RESOURCE_PATH_BYTES: usize = 4096;
const MAX_RESOURCE_PATH_SEGMENTS: usize = 256;

/// Root directory listing response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    /// The current resource path. The Root Directory is represented by an empty path.
    pub path: String,
    /// Active listing sort.
    pub sort: ListingSort,
    /// Active Current List Filter state.
    pub filter: CurrentListFilter,
    /// Clickable current-path breadcrumb segments.
    pub breadcrumbs: Vec<BreadcrumbSegment>,
    /// Direct child resources under the current directory.
    pub resources: Vec<ResourceRow>,
}

/// Current List Filter state.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentListFilter {
    /// Case-insensitive resource name substring used to narrow the current Directory listing.
    pub query: String,
}

/// Active listing sort state.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListingSort {
    /// Active sort field.
    pub field: SortField,
    /// Active sort order.
    pub order: SortOrder,
}

/// A resource listing sort field.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SortField {
    /// Sort by Resource Name.
    #[serde(rename = "name")]
    Name,
    /// Sort by File Size.
    #[serde(rename = "size")]
    Size,
    /// Sort by Modified Time.
    ModifiedTime,
}

/// A resource listing sort order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SortOrder {
    /// Ascending order.
    #[serde(rename = "asc")]
    Asc,
    /// Descending order.
    #[serde(rename = "desc")]
    Desc,
}

/// A clickable Breadcrumb segment.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BreadcrumbSegment {
    /// Browser-visible segment label.
    pub label: String,
    /// Segment target resource path. The Root Directory is represented by an empty path.
    pub path: String,
}

/// A browser-visible resource row.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRow {
    /// Resource name within its containing Directory.
    pub name: String,
    /// Resource path relative to the storage root.
    pub resource_path: String,
    /// Resource kind.
    pub kind: ResourceKind,
    /// File size in bytes. Directories omit this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Entry modified time formatted in the configured server time zone.
    pub modified_time: String,
}

/// A listed resource kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResourceKind {
    /// A directory resource.
    Directory,
    /// A regular file resource.
    File,
}

/// File Resource download payload.
#[derive(Debug)]
pub struct FileDownload {
    /// File Resource Name used as the suggested Download Name.
    pub download_name: String,
    /// File size in bytes.
    pub content_length: u64,
    /// Downloaded file content.
    pub content: File,
}

/// Directory Archive download payload.
#[derive(Debug)]
pub struct ArchiveDownload {
    /// Directory Archive Download Name.
    pub download_name: String,
    /// Archive payload size in bytes.
    pub content_length: u64,
    /// Complete zip archive bytes.
    pub content: Vec<u8>,
}

/// Resource listing failure.
#[derive(Debug, Error)]
pub enum ResourceError {
    /// The requested resource path is invalid.
    #[error("resource path is invalid")]
    InvalidResourcePath,
    /// The Current List Filter query is invalid.
    #[error("current list filter query is invalid")]
    InvalidFilter,
    /// The Server Search query is invalid.
    #[error("server search query is invalid")]
    InvalidSearchQuery,
    /// The requested resource path is not a directory resource.
    #[error("resource path is not a directory")]
    NotDirectory,
    /// The requested resource path is not a file resource.
    #[error("resource path is not a file")]
    NotFile,
    /// The Root Directory cannot be downloaded as an archive.
    #[error("root directory cannot be downloaded as an archive")]
    RootDirectoryArchive,
    /// The requested resource path does not exist.
    #[error("resource path does not exist")]
    ResourceNotFound,
    /// The configured directory contains more resources than the direct child limit.
    #[error("direct child listing exceeds configured limit of {limit}")]
    ListingLimitExceeded {
        /// Configured direct child resource limit.
        limit: usize,
    },
    /// A Directory Archive would contain more resources than configured.
    #[error("directory archive exceeds configured resource count limit of {limit}")]
    ArchiveResourceCountLimitExceeded {
        /// Configured archive resource count limit.
        limit: usize,
    },
    /// A Directory Archive would contain more uncompressed bytes than configured.
    #[error("directory archive exceeds configured uncompressed size limit of {limit} bytes")]
    ArchiveSizeLimitExceeded {
        /// Configured uncompressed archive byte limit.
        limit: u64,
    },
    /// The storage root could not be read.
    #[error("failed to read directory")]
    ReadDirectory(#[source] std::io::Error),
    /// A directory entry could not be read.
    #[error("failed to read directory entry")]
    ReadEntry(#[source] std::io::Error),
    /// A resource had a name that cannot be represented safely.
    #[error("resource name is not valid UTF-8")]
    InvalidResourceName,
    /// A resource's metadata could not be read.
    #[error("failed to read resource metadata")]
    Metadata(#[source] std::io::Error),
    /// A resource's modified time could not be read.
    #[error("failed to read resource modified time")]
    ModifiedTime(#[source] std::io::Error),
    /// A file resource's content could not be read.
    #[error("failed to read file resource")]
    ReadFile(#[source] std::io::Error),
    /// Directory Archive generation failed while reading file content.
    #[error("failed to read archive resource")]
    ReadArchiveFile(#[source] std::io::Error),
    /// Directory Archive zip metadata generation failed.
    #[error("failed to build directory archive")]
    ZipArchive(#[source] zip::result::ZipError),
    /// Directory Archive zip bytes could not be written.
    #[error("failed to write directory archive")]
    WriteArchive(#[source] std::io::Error),
    /// Directory Archive payload length exceeded supported response metadata.
    #[error("directory archive length is unsupported")]
    ArchiveLengthOverflow,
    /// A Resource Name supplied for a write action is invalid.
    #[error("resource name is invalid")]
    InvalidWriteResourceName,
    /// A Resource already exists at the requested destination.
    #[error("resource name conflicts with an existing resource")]
    NameConflict,
    /// A Directory Resource could not be created.
    #[error("failed to create directory resource")]
    CreateDirectory(#[source] std::io::Error),
    /// One uploaded File exceeded the configured byte limit.
    #[error("uploaded file exceeds configured size limit of {limit} bytes")]
    UploadSingleFileSizeLimitExceeded {
        /// Configured single File byte limit.
        limit: u64,
    },
    /// One upload request exceeded the configured aggregate byte limit.
    #[error("upload exceeds configured total size limit of {limit} bytes")]
    UploadTotalSizeLimitExceeded {
        /// Configured aggregate upload byte limit.
        limit: u64,
    },
    /// Staging or publishing an uploaded File failed.
    #[error("failed to store uploaded file")]
    StoreUpload(#[source] std::io::Error),
}

/// A File being written in the reserved staging area before atomic publication.
#[derive(Debug)]
pub struct StagedFileUpload {
    file: File,
    staging_directory: Dir,
    destination_directory: Dir,
    staging_name: String,
    destination_name: String,
    cleanup_staging: bool,
    bytes_written: u64,
    single_file_limit: u64,
    total_upload_limit: u64,
}

/// List direct resources in the Root Directory.
///
/// # Errors
///
/// Returns an error when the storage root cannot be read, resource metadata is unavailable,
/// resource names are invalid, or the configured listing limit is exceeded.
pub async fn list_root_directory(config: &AppConfig) -> Result<DirectoryListing, ResourceError> {
    list_directory(
        config,
        "",
        ListingSort::default(),
        CurrentListFilter::default(),
    )
    .await
}

/// Create a Directory under the current Resource Path.
///
/// # Errors
///
/// Returns an error when the Resource Path or Resource Name is invalid, the destination already
/// exists, or the Directory cannot be created.
pub async fn create_directory(
    config: &AppConfig,
    path: &str,
    name: &str,
) -> Result<(), ResourceError> {
    let resource_path = ResourcePath::parse(path)?;
    if resource_path.contains_reserved_name(config.staging_directory_name()) {
        return Err(ResourceError::InvalidResourcePath);
    }
    if !is_valid_resource_name(name) || name == config.staging_directory_name() {
        return Err(ResourceError::InvalidWriteResourceName);
    }

    let storage_root = config.storage_root().to_path_buf();
    let segments = owned_segments(&resource_path);
    let name = name.to_owned();
    task::spawn_blocking(move || {
        let root = Dir::open_ambient_dir(storage_root, ambient_authority())
            .map_err(ResourceError::CreateDirectory)?;
        let parent = open_relative_directory(&root, &segments)?;
        match parent.create_dir(name) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(ResourceError::NameConflict)
            }
            Err(error) => Err(ResourceError::CreateDirectory(error)),
        }
    })
    .await
    .map_err(blocking_task_error)??;
    Ok(())
}

impl StagedFileUpload {
    /// Start staging one File for upload into the current Resource Path.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid path or name, an existing destination, or a staging IO
    /// failure.
    pub async fn start(config: &AppConfig, path: &str, name: &str) -> Result<Self, ResourceError> {
        let resource_path = ResourcePath::parse(path)?;
        if resource_path.contains_reserved_name(config.staging_directory_name()) {
            return Err(ResourceError::InvalidResourcePath);
        }
        if !is_valid_resource_name(name) || name == config.staging_directory_name() {
            return Err(ResourceError::InvalidWriteResourceName);
        }

        let storage_root = config.storage_root().to_path_buf();
        let segments = owned_segments(&resource_path);
        let destination_name = name.to_owned();
        let staging_directory_name = config.staging_directory_name().to_owned();
        let (file, staging_directory, destination_directory, staging_name) =
            task::spawn_blocking(move || {
                let root = Dir::open_ambient_dir(storage_root, ambient_authority())
                    .map_err(ResourceError::StoreUpload)?;
                let destination_directory = open_relative_directory(&root, &segments)?;
                match destination_directory.symlink_metadata(&destination_name) {
                    Ok(_) => return Err(ResourceError::NameConflict),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(ResourceError::StoreUpload(error)),
                }
                let staging_directory = open_staging_directory(&root, &staging_directory_name)?;
                let staging_name = create_staging_name()?;
                let mut options = CapOpenOptions::new();
                options.write(true).create_new(true);
                let file = staging_directory
                    .open_with(&staging_name, &options)
                    .map_err(ResourceError::StoreUpload)?;
                Ok((
                    File::from_std(file.into_std()),
                    staging_directory,
                    destination_directory,
                    staging_name,
                ))
            })
            .await
            .map_err(blocking_task_error)??;
        Ok(Self {
            file,
            staging_directory,
            destination_directory,
            staging_name,
            destination_name: name.to_owned(),
            cleanup_staging: true,
            bytes_written: 0,
            single_file_limit: config.limits().upload_single_file_size_limit_bytes().get(),
            total_upload_limit: config.limits().upload_total_size_limit_bytes().get(),
        })
    }

    /// Append one multipart chunk to the staged File.
    ///
    /// # Errors
    ///
    /// Returns an error when a configured upload limit is exceeded or staging IO fails.
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), ResourceError> {
        let chunk_length = u64::try_from(chunk.len()).map_err(|_| {
            ResourceError::UploadSingleFileSizeLimitExceeded {
                limit: self.single_file_limit,
            }
        })?;
        let next_length = self.bytes_written.checked_add(chunk_length).ok_or(
            ResourceError::UploadSingleFileSizeLimitExceeded {
                limit: self.single_file_limit,
            },
        )?;
        if next_length > self.single_file_limit {
            return Err(ResourceError::UploadSingleFileSizeLimitExceeded {
                limit: self.single_file_limit,
            });
        }
        if next_length > self.total_upload_limit {
            return Err(ResourceError::UploadTotalSizeLimitExceeded {
                limit: self.total_upload_limit,
            });
        }
        self.file
            .write_all(chunk)
            .await
            .map_err(ResourceError::StoreUpload)?;
        self.bytes_written = next_length;
        Ok(())
    }

    /// Publish the complete staged File atomically without replacing an existing Resource.
    ///
    /// # Errors
    ///
    /// Returns an error when the destination conflicts or filesystem publication fails.
    pub async fn commit(mut self) -> Result<(), ResourceError> {
        if let Err(error) = self.file.sync_all().await {
            return Err(ResourceError::StoreUpload(error));
        }
        let staging_directory = self
            .staging_directory
            .try_clone()
            .map_err(ResourceError::StoreUpload)?;
        let destination_directory = self
            .destination_directory
            .try_clone()
            .map_err(ResourceError::StoreUpload)?;
        let staging_name = self.staging_name.clone();
        let destination_name = self.destination_name.clone();
        let publish = task::spawn_blocking(move || {
            staging_directory.hard_link(&staging_name, &destination_directory, &destination_name)
        })
        .await
        .map_err(blocking_task_error)?;
        match publish {
            Ok(()) => {
                if let Err(error) = self.remove_staging().await {
                    warn!(%error, staging_name = %self.staging_name, "failed to remove published staging file");
                }
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(ResourceError::NameConflict)
            }
            Err(error) => Err(ResourceError::StoreUpload(error)),
        }
    }

    /// Remove a staged File after parsing or validation fails.
    pub async fn abort(mut self) {
        let _cleanup_result = self.remove_staging().await;
    }

    async fn remove_staging(&mut self) -> Result<(), std::io::Error> {
        let staging_directory = self.staging_directory.try_clone()?;
        let staging_name = self.staging_name.clone();
        task::spawn_blocking(move || staging_directory.remove_file(staging_name))
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))??;
        self.cleanup_staging = false;
        Ok(())
    }
}

impl Drop for StagedFileUpload {
    fn drop(&mut self) {
        // Drop is the cancellation path; a synchronous unlink also works during runtime shutdown.
        if self.cleanup_staging
            && let Err(error) = self.staging_directory.remove_file(&self.staging_name)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(%error, staging_name = %self.staging_name, "failed to clean up staged upload");
        }
    }
}

fn create_staging_name() -> Result<String, ResourceError> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|error| ResourceError::StoreUpload(std::io::Error::other(error.to_string())))?;
    let mut name = String::with_capacity(random.len() * 2);
    for byte in random {
        write!(&mut name, "{byte:02x}").map_err(|error| {
            ResourceError::StoreUpload(std::io::Error::other(error.to_string()))
        })?;
    }
    Ok(name)
}

fn owned_segments(resource_path: &ResourcePath<'_>) -> Vec<String> {
    resource_path
        .segments
        .iter()
        .map(|segment| (*segment).to_owned())
        .collect()
}

fn open_relative_directory(root: &Dir, segments: &[String]) -> Result<Dir, ResourceError> {
    let mut directory = root.try_clone().map_err(ResourceError::ReadDirectory)?;
    for segment in segments {
        let metadata = directory
            .symlink_metadata(segment)
            .map_err(map_resolve_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ResourceError::NotDirectory);
        }
        directory = directory.open_dir(segment).map_err(map_resolve_error)?;
    }
    Ok(directory)
}

fn open_staging_directory(root: &Dir, name: &str) -> Result<Dir, ResourceError> {
    match root.create_dir(name) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(ResourceError::StoreUpload(error)),
    }
    let metadata = root
        .symlink_metadata(name)
        .map_err(ResourceError::StoreUpload)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ResourceError::StoreUpload(std::io::Error::other(
            "reserved staging path is not a directory",
        )));
    }
    root.open_dir(name).map_err(ResourceError::StoreUpload)
}

fn blocking_task_error(error: task::JoinError) -> ResourceError {
    ResourceError::StoreUpload(std::io::Error::other(error))
}

/// List direct resources in a Directory.
///
/// # Errors
///
/// Returns an error when the resource path is invalid, the directory cannot be read, resource
/// metadata is unavailable, resource names are invalid, or the configured listing limit is
/// exceeded.
pub async fn list_directory(
    config: &AppConfig,
    path: &str,
    sort: ListingSort,
    filter: CurrentListFilter,
) -> Result<DirectoryListing, ResourceError> {
    let resource_path = ResourcePath::parse(path)?;
    if resource_path.contains_reserved_name(config.staging_directory_name()) {
        return Err(ResourceError::InvalidResourcePath);
    }
    filter.validate()?;
    let directory_path = resolve_directory_path(config.storage_root(), &resource_path).await?;

    let mut read_dir = fs::read_dir(&directory_path)
        .await
        .map_err(ResourceError::ReadDirectory)?;
    let mut resources = Vec::new();
    let limit = config.limits().listing_direct_child_limit().get();

    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(ResourceError::ReadEntry)?
    {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| ResourceError::InvalidResourceName)?;
        if name == config.staging_directory_name() {
            continue;
        }

        let metadata = fs::symlink_metadata(entry.path())
            .await
            .map_err(ResourceError::Metadata)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            continue;
        }

        let kind = if file_type.is_dir() {
            ResourceKind::Directory
        } else if file_type.is_file() {
            ResourceKind::File
        } else {
            continue;
        };

        let child_path = resource_path.join_name(&name);
        resources.push(ResourceRow {
            resource_path: child_path,
            name,
            kind,
            size: (kind == ResourceKind::File).then_some(metadata.len()),
            modified_time: format_modified_time(
                metadata.modified().map_err(ResourceError::ModifiedTime)?,
                config.server().time_zone(),
            ),
        });

        if resources.len() > limit {
            return Err(ResourceError::ListingLimitExceeded { limit });
        }
    }

    filter.apply(&mut resources);
    resources.sort_by(|left, right| compare_resource_rows(left, right, sort));

    Ok(DirectoryListing {
        path: resource_path.as_str().to_owned(),
        sort,
        filter,
        breadcrumbs: resource_path.breadcrumbs(),
        resources,
    })
}

/// Download a File Resource by Resource Path.
///
/// # Errors
///
/// Returns an error when the resource path is invalid, missing, outside the storage root, points
/// at a Directory or symbolic link, or the file content cannot be opened.
pub async fn download_file(config: &AppConfig, path: &str) -> Result<FileDownload, ResourceError> {
    let resource_path = ResourcePath::parse(path)?;
    if resource_path.as_str().is_empty() {
        return Err(ResourceError::InvalidResourcePath);
    }
    if resource_path.contains_reserved_name(config.staging_directory_name()) {
        return Err(ResourceError::InvalidResourcePath);
    }

    let (file_path, content_length) =
        resolve_file_path(config.storage_root(), &resource_path).await?;
    let content = File::open(file_path)
        .await
        .map_err(ResourceError::ReadFile)?;

    Ok(FileDownload {
        download_name: resource_path.file_name()?,
        content_length,
        content,
    })
}

/// Download a Directory Resource as a zip archive by Resource Path.
///
/// # Errors
///
/// Returns an error when the resource path is invalid, points at the Root Directory, contains the
/// reserved staging directory, is not a Directory Resource, exceeds archive limits, or archive
/// bytes cannot be generated.
pub async fn download_directory_archive(
    config: &AppConfig,
    path: &str,
) -> Result<ArchiveDownload, ResourceError> {
    let resource_path = ResourcePath::parse(path)?;
    if resource_path.as_str().is_empty() {
        return Err(ResourceError::RootDirectoryArchive);
    }
    if resource_path.contains_reserved_name(config.staging_directory_name()) {
        return Err(ResourceError::InvalidResourcePath);
    }

    let directory_path = resolve_directory_path(config.storage_root(), &resource_path).await?;
    let directory_name = resource_path.file_name()?;
    let entries = collect_archive_entries(config, directory_path, directory_name.clone()).await?;
    let content = build_archive(entries).await?;
    let content_length =
        u64::try_from(content.len()).map_err(|_| ResourceError::ArchiveLengthOverflow)?;

    Ok(ArchiveDownload {
        download_name: format!("{directory_name}.zip"),
        content_length,
        content,
    })
}

/// Search Resources by Resource Name across the Resource tree.
///
/// # Errors
///
/// Returns an error when the query is shorter than two non-whitespace characters or exceeds the
/// maximum query length.
pub async fn search_resources(
    config: &AppConfig,
    query: &str,
) -> Result<SearchResults, ResourceError> {
    validate_search_query(query)?;
    let query = query.trim();
    let query_lowercase = query.to_lowercase();
    let mut resources = Vec::new();
    let mut pending_directories = vec![(config.storage_root().to_path_buf(), String::new())];
    let result_limit = config.limits().search_result_limit().get();
    let traversal_limit = config.limits().search_traversal_limit().get();
    let mut traversed_resources = 0usize;
    let mut truncated = false;

    while let Some((current_directory, containing_path)) = pending_directories.pop() {
        let mut read_dir = fs::read_dir(&current_directory)
            .await
            .map_err(ResourceError::ReadDirectory)?;
        let mut entries = Vec::new();

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(ResourceError::ReadEntry)?
        {
            entries.push(entry);
        }
        entries.sort_by_key(tokio::fs::DirEntry::file_name);

        for entry in entries {
            if traversed_resources == traversal_limit {
                truncated = true;
                pending_directories.clear();
                break;
            }
            traversed_resources += 1;

            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ResourceError::InvalidResourceName)?;
            if name == config.staging_directory_name() {
                continue;
            }

            let metadata = fs::symlink_metadata(entry.path())
                .await
                .map_err(ResourceError::Metadata)?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }

            let kind = if file_type.is_dir() {
                ResourceKind::Directory
            } else if file_type.is_file() {
                ResourceKind::File
            } else {
                continue;
            };
            let resource_path = join_resource_path(&containing_path, &name);

            if name.to_lowercase().contains(&query_lowercase) {
                if resources.len() == result_limit {
                    truncated = true;
                    pending_directories.clear();
                    break;
                }

                resources.push(SearchResultRow {
                    resource: ResourceRow {
                        resource_path: resource_path.clone(),
                        name: name.clone(),
                        kind,
                        size: (kind == ResourceKind::File).then_some(metadata.len()),
                        modified_time: format_modified_time(
                            metadata.modified().map_err(ResourceError::ModifiedTime)?,
                            config.server().time_zone(),
                        ),
                    },
                    containing_path: containing_path.clone(),
                });
            }

            if kind == ResourceKind::Directory {
                pending_directories.push((entry.path(), resource_path));
            }
        }
    }

    resources.sort_by(|left, right| {
        left.resource
            .resource_path
            .cmp(&right.resource.resource_path)
    });
    Ok(SearchResults {
        query: query.to_owned(),
        truncated,
        resources,
    })
}

/// Server Search response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    /// Validated Server Search query.
    pub query: String,
    /// Whether the configured limits prevented a complete result set.
    pub truncated: bool,
    /// Flat Search Results.
    pub resources: Vec<SearchResultRow>,
}

/// A flat Server Search result row.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultRow {
    /// Matched Resource.
    pub resource: ResourceRow,
    /// Containing Resource Path.
    pub containing_path: String,
}

#[derive(Debug)]
struct ResourcePath<'a> {
    raw: &'a str,
    segments: Vec<&'a str>,
}

impl<'a> ResourcePath<'a> {
    fn parse(raw: &'a str) -> Result<Self, ResourceError> {
        if raw.len() > MAX_RESOURCE_PATH_BYTES {
            return Err(ResourceError::InvalidResourcePath);
        }
        if raw.is_empty() {
            return Ok(Self {
                raw,
                segments: Vec::new(),
            });
        }

        let segments: Vec<&str> = raw.split('/').collect();
        if segments.len() > MAX_RESOURCE_PATH_SEGMENTS
            || segments
                .iter()
                .any(|segment| !is_valid_resource_name(segment))
        {
            return Err(ResourceError::InvalidResourcePath);
        }

        Ok(Self { raw, segments })
    }

    fn as_str(&self) -> &str {
        self.raw
    }

    fn join_name(&self, name: &str) -> String {
        if self.raw.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.raw)
        }
    }

    fn breadcrumbs(&self) -> Vec<BreadcrumbSegment> {
        let mut breadcrumbs = Vec::with_capacity(self.segments.len() + 1);
        breadcrumbs.push(BreadcrumbSegment {
            label: "Root Directory".to_owned(),
            path: String::new(),
        });

        let mut path = String::new();
        for segment in &self.segments {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(segment);
            breadcrumbs.push(BreadcrumbSegment {
                label: (*segment).to_owned(),
                path: path.clone(),
            });
        }

        breadcrumbs
    }

    fn contains_reserved_name(&self, reserved_name: &str) -> bool {
        self.segments.contains(&reserved_name)
    }

    fn file_name(&self) -> Result<String, ResourceError> {
        self.segments
            .last()
            .map(|name| (*name).to_owned())
            .ok_or(ResourceError::NotFile)
    }
}

#[derive(Debug)]
struct ArchiveEntry {
    archive_path: String,
    filesystem_path: PathBuf,
    kind: ArchiveEntryKind,
}

#[derive(Debug)]
enum ArchiveEntryKind {
    Directory,
    File,
}

async fn resolve_directory_path(
    storage_root: &std::path::Path,
    resource_path: &ResourcePath<'_>,
) -> Result<PathBuf, ResourceError> {
    let mut path = storage_root.to_path_buf();
    for segment in &resource_path.segments {
        path.push(segment);
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(map_resolve_error)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(ResourceError::NotDirectory);
        }
    }

    let canonical = fs::canonicalize(path).await.map_err(map_resolve_error)?;
    if !canonical.starts_with(storage_root) {
        return Err(ResourceError::InvalidResourcePath);
    }

    Ok(canonical)
}

async fn collect_archive_entries(
    config: &AppConfig,
    directory_path: PathBuf,
    directory_name: String,
) -> Result<Vec<ArchiveEntry>, ResourceError> {
    let resource_count_limit = config.limits().archive_resource_count_limit().get();
    let size_limit = config
        .limits()
        .archive_uncompressed_size_limit_bytes()
        .get();
    let mut resource_count = 1usize;
    let mut uncompressed_size = 0u64;
    let mut entries = vec![ArchiveEntry {
        archive_path: format!("{directory_name}/"),
        filesystem_path: directory_path.clone(),
        kind: ArchiveEntryKind::Directory,
    }];
    let mut pending_directories = vec![(directory_path, directory_name)];

    while let Some((current_directory, current_archive_path)) = pending_directories.pop() {
        let mut read_dir = fs::read_dir(&current_directory)
            .await
            .map_err(ResourceError::ReadDirectory)?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(ResourceError::ReadEntry)?
        {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ResourceError::InvalidResourceName)?;
            if name == config.staging_directory_name() {
                continue;
            }

            let metadata = fs::symlink_metadata(entry.path())
                .await
                .map_err(ResourceError::Metadata)?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }

            if !file_type.is_dir() && !file_type.is_file() {
                continue;
            }

            resource_count = resource_count.checked_add(1).ok_or(
                ResourceError::ArchiveResourceCountLimitExceeded {
                    limit: resource_count_limit,
                },
            )?;
            if resource_count > resource_count_limit {
                return Err(ResourceError::ArchiveResourceCountLimitExceeded {
                    limit: resource_count_limit,
                });
            }

            let child_archive_path = format!("{current_archive_path}/{name}");
            if file_type.is_dir() {
                entries.push(ArchiveEntry {
                    archive_path: format!("{child_archive_path}/"),
                    filesystem_path: entry.path(),
                    kind: ArchiveEntryKind::Directory,
                });
                pending_directories.push((entry.path(), child_archive_path));
            } else {
                uncompressed_size = uncompressed_size
                    .checked_add(metadata.len())
                    .ok_or(ResourceError::ArchiveSizeLimitExceeded { limit: size_limit })?;
                if uncompressed_size > size_limit {
                    return Err(ResourceError::ArchiveSizeLimitExceeded { limit: size_limit });
                }

                entries.push(ArchiveEntry {
                    archive_path: child_archive_path,
                    filesystem_path: entry.path(),
                    kind: ArchiveEntryKind::File,
                });
            }
        }
    }

    entries.sort_by(|left, right| left.archive_path.cmp(&right.archive_path));
    Ok(entries)
}

async fn build_archive(entries: Vec<ArchiveEntry>) -> Result<Vec<u8>, ResourceError> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    for entry in entries {
        match entry.kind {
            ArchiveEntryKind::Directory => writer
                .add_directory(entry.archive_path, options)
                .map_err(ResourceError::ZipArchive)?,
            ArchiveEntryKind::File => {
                let content = fs::read(entry.filesystem_path)
                    .await
                    .map_err(ResourceError::ReadArchiveFile)?;
                writer
                    .start_file(entry.archive_path, options)
                    .map_err(ResourceError::ZipArchive)?;
                writer
                    .write_all(&content)
                    .map_err(ResourceError::WriteArchive)?;
            }
        }
    }

    let cursor = writer.finish().map_err(ResourceError::ZipArchive)?;
    Ok(cursor.into_inner())
}

async fn resolve_file_path(
    storage_root: &std::path::Path,
    resource_path: &ResourcePath<'_>,
) -> Result<(PathBuf, u64), ResourceError> {
    let Some((file_name, parent_segments)) = resource_path.segments.split_last() else {
        return Err(ResourceError::NotFile);
    };

    let mut path = storage_root.to_path_buf();
    for segment in parent_segments {
        path.push(segment);
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(map_resolve_error)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(ResourceError::NotDirectory);
        }
    }

    path.push(file_name);
    let metadata = fs::symlink_metadata(&path)
        .await
        .map_err(map_resolve_error)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(ResourceError::NotFile);
    }

    let canonical = fs::canonicalize(path).await.map_err(map_resolve_error)?;
    if !canonical.starts_with(storage_root) {
        return Err(ResourceError::InvalidResourcePath);
    }

    Ok((canonical, metadata.len()))
}

fn map_resolve_error(error: std::io::Error) -> ResourceError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ResourceError::ResourceNotFound
    } else {
        ResourceError::ReadDirectory(error)
    }
}

fn validate_search_query(query: &str) -> Result<(), ResourceError> {
    let non_whitespace_characters = query
        .chars()
        .filter(|character| !character.is_whitespace())
        .take(2)
        .count();
    if non_whitespace_characters < 2 || query.len() > MAX_SEARCH_QUERY_BYTES {
        return Err(ResourceError::InvalidSearchQuery);
    }

    Ok(())
}

fn join_resource_path(containing_path: &str, name: &str) -> String {
    if containing_path.is_empty() {
        name.to_owned()
    } else {
        format!("{containing_path}/{name}")
    }
}

fn is_valid_resource_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_RESOURCE_NAME_BYTES
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.chars().any(char::is_control)
}

impl Default for ListingSort {
    fn default() -> Self {
        Self {
            field: SortField::Name,
            order: SortOrder::Asc,
        }
    }
}

impl CurrentListFilter {
    fn validate(&self) -> Result<(), ResourceError> {
        if self.query.len() > 256 {
            return Err(ResourceError::InvalidFilter);
        }

        Ok(())
    }

    fn apply(&self, resources: &mut Vec<ResourceRow>) {
        let query = self.query.trim();
        if query.is_empty() {
            return;
        }

        let query = query.to_lowercase();
        resources.retain(|resource| resource.name.to_lowercase().contains(&query));
    }
}

fn compare_resource_rows(left: &ResourceRow, right: &ResourceRow, sort: ListingSort) -> Ordering {
    resource_kind_rank(left.kind)
        .cmp(&resource_kind_rank(right.kind))
        .then_with(|| compare_by_sort_field(left, right, sort))
        .then_with(|| left.name.cmp(&right.name))
}

fn compare_by_sort_field(left: &ResourceRow, right: &ResourceRow, sort: ListingSort) -> Ordering {
    if sort.field == SortField::Size && left.kind == ResourceKind::Directory {
        return left.name.cmp(&right.name);
    }

    let ordering = match sort.field {
        SortField::Name => left.name.cmp(&right.name),
        SortField::Size => left.size.cmp(&right.size),
        SortField::ModifiedTime => left.modified_time.cmp(&right.modified_time),
    };

    match sort.order {
        SortOrder::Asc => ordering,
        SortOrder::Desc => ordering.reverse(),
    }
}

fn resource_kind_rank(kind: ResourceKind) -> u8 {
    match kind {
        ResourceKind::Directory => 0,
        ResourceKind::File => 1,
    }
}

fn format_modified_time(modified_time: std::time::SystemTime, time_zone: Tz) -> String {
    let utc_time = DateTime::<Utc>::from(modified_time);
    utc_time
        .with_timezone(&time_zone)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}
