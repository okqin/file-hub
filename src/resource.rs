//! Storage-root-backed resource listing.

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "redox",
    target_vendor = "apple"
))]
use std::os::fd::AsFd;
use std::{
    cmp::Ordering,
    io::{Cursor, Write},
    path::PathBuf,
};

#[cfg(any(target_os = "android", target_os = "linux", target_vendor = "apple"))]
use cap_std::fs::MetadataExt;
use cap_std::{ambient_authority, fs::Dir};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    fs::{self, File},
    task,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{
    config::AppConfig,
    resource_address::{ResourceName, ResourcePath},
};

const MAX_SEARCH_QUERY_BYTES: usize = 256;

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
    /// The Root Directory cannot be renamed.
    #[error("root directory cannot be renamed")]
    RootDirectoryRename,
    /// The Root Directory cannot be deleted.
    #[error("root directory cannot be deleted")]
    RootDirectoryDelete,
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
    /// A File or Directory Resource could not be renamed.
    #[error("failed to rename resource")]
    RenameResource(#[source] std::io::Error),
    /// A File or Directory Resource could not be deleted.
    #[error("failed to {operation} at {path}")]
    DeleteResource {
        /// Resource Path affected by the first failure.
        path: String,
        /// Filesystem operation that failed.
        operation: &'static str,
        /// Underlying filesystem reason.
        #[source]
        source: std::io::Error,
    },
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
    let resource_path = parse_user_path(config, path)?;
    let name = parse_user_name(config, name)?;
    resource_path
        .join(&name)
        .map_err(|_| ResourceError::InvalidResourcePath)?;

    let storage_root = config.storage_root().to_path_buf();
    let segments = resource_path.segments().to_vec();
    task::spawn_blocking(move || {
        let root = Dir::open_ambient_dir(storage_root, ambient_authority())
            .map_err(ResourceError::CreateDirectory)?;
        let parent = open_relative_directory(&root, &segments)?;
        match parent.create_dir(name.as_str()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(ResourceError::NameConflict)
            }
            Err(error) => Err(ResourceError::CreateDirectory(error)),
        }
    })
    .await
    .map_err(create_directory_task_error)??;
    Ok(())
}

/// Rename a File or Directory Resource within its containing Directory.
///
/// # Errors
///
/// Returns an error when the Resource Path or new Resource Name is invalid, the source is not a
/// regular File or Directory, the destination already exists, or the atomic rename fails.
pub async fn rename_resource(
    config: &AppConfig,
    path: &str,
    new_name: &str,
) -> Result<(), ResourceError> {
    let resource_path = parse_user_path(config, path)?;
    let new_name = parse_user_name(config, new_name)?;
    let Some((source_name, parent_segments)) = resource_path.segments().split_last() else {
        return Err(ResourceError::RootDirectoryRename);
    };
    let parent_path = resource_path
        .parent()
        .ok_or(ResourceError::RootDirectoryRename)?;
    parent_path
        .join(&new_name)
        .map_err(|_| ResourceError::InvalidWriteResourceName)?;

    let storage_root = config.storage_root().to_path_buf();
    let parent_segments = parent_segments.to_vec();
    let source_name = source_name.clone();
    task::spawn_blocking(move || {
        let root = Dir::open_ambient_dir(storage_root, ambient_authority())
            .map_err(ResourceError::RenameResource)?;
        let parent = open_relative_directory(&root, &parent_segments)?;
        let metadata = parent
            .symlink_metadata(source_name.as_str())
            .map_err(map_resolve_error)?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(ResourceError::InvalidResourcePath);
        }
        if source_name == new_name {
            return Ok(());
        }
        match rename_noreplace(&parent, source_name.as_str(), &parent, new_name.as_str()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(ResourceError::NameConflict)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(ResourceError::ResourceNotFound)
            }
            Err(error) => Err(ResourceError::RenameResource(error)),
        }
    })
    .await
    .map_err(|error| ResourceError::RenameResource(std::io::Error::other(error)))??;
    Ok(())
}

/// Delete a File Resource or recursively delete a Directory Resource.
///
/// # Errors
///
/// Returns an error when the Resource Path is invalid, the target is not a regular File or
/// Directory, or the target cannot be removed.
pub async fn delete_resource(config: &AppConfig, path: &str) -> Result<(), ResourceError> {
    let resource_path = parse_user_path(config, path)?;
    let Some((target_name, parent_segments)) = resource_path.segments().split_last() else {
        return Err(ResourceError::RootDirectoryDelete);
    };

    let storage_root = config.storage_root().to_path_buf();
    let parent_segments = parent_segments.to_vec();
    let target_name = target_name.clone();
    let requested_path = path.to_owned();
    let task_path = requested_path.clone();
    task::spawn_blocking(move || {
        let root = Dir::open_ambient_dir(storage_root, ambient_authority())
            .map_err(|error| delete_failure(&task_path, "open storage root", error))?;
        let parent = open_relative_directory(&root, &parent_segments)?;
        let metadata = parent
            .symlink_metadata(target_name.as_str())
            .map_err(map_resolve_error)?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(ResourceError::InvalidResourcePath);
        }
        if metadata.is_dir() {
            remove_directory_resource(&parent, target_name.as_str(), &task_path, &metadata)?;
        } else {
            parent
                .remove_file(target_name.as_str())
                .map_err(|error| delete_failure(&task_path, "delete File", error))?;
        }
        match parent.symlink_metadata(target_name.as_str()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(delete_failure(&task_path, "verify deleted Resource", error)),
            Ok(_) => Err(delete_failure(
                &task_path,
                "verify deleted Resource",
                std::io::Error::other("target Resource still exists after deletion"),
            )),
        }
    })
    .await
    .map_err(|error| delete_failure(&requested_path, "complete Delete", error.into()))??;
    Ok(())
}

fn remove_directory_resource(
    parent: &Dir,
    name: &str,
    resource_path: &str,
    expected_metadata: &cap_std::fs::Metadata,
) -> Result<(), ResourceError> {
    let directory = open_directory_nofollow(parent, name, expected_metadata)
        .map_err(|error| delete_failure(resource_path, "open Directory", error))?;
    let entries = directory
        .entries()
        .map_err(|error| delete_failure(resource_path, "read Directory", error))?;

    for entry in entries {
        let entry =
            entry.map_err(|error| delete_failure(resource_path, "read Directory", error))?;
        let child_name = entry.file_name();
        let child_name = child_name.to_str().ok_or_else(|| {
            delete_failure(
                resource_path,
                "read Resource Name",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Resource Name is not valid UTF-8",
                ),
            )
        })?;
        let parent_path = ResourcePath::try_from(resource_path).map_err(|_| {
            delete_failure(
                resource_path,
                "validate containing Resource Path",
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Resource Path is invalid"),
            )
        })?;
        let child_name = ResourceName::try_from(child_name).map_err(|_| {
            delete_failure(
                resource_path,
                "validate child Resource Name",
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Resource Name is invalid"),
            )
        })?;
        let child_path = parent_path
            .join(&child_name)
            .map_err(|_| {
                delete_failure(
                    resource_path,
                    "build child Resource Path",
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Resource Path exceeds configured limits",
                    ),
                )
            })?
            .as_str()
            .to_owned();
        let metadata = directory
            .symlink_metadata(child_name.as_str())
            .map_err(|error| delete_failure(&child_path, "read Resource metadata", error))?;
        if metadata.file_type().is_symlink() {
            return Err(delete_failure(
                &child_path,
                "delete symbolic link",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "symbolic links are not Resources",
                ),
            ));
        }
        if metadata.is_dir() {
            remove_directory_resource(&directory, child_name.as_str(), &child_path, &metadata)?;
        } else if metadata.is_file() {
            directory
                .remove_file(child_name.as_str())
                .map_err(|error| delete_failure(&child_path, "delete File", error))?;
        } else {
            return Err(delete_failure(
                &child_path,
                "delete unsupported filesystem entry",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "filesystem entry is not a Resource",
                ),
            ));
        }
    }

    parent
        .remove_dir(name)
        .map_err(|error| delete_failure(resource_path, "delete Directory", error))
}

#[cfg(any(target_os = "android", target_os = "linux", target_vendor = "apple"))]
fn open_directory_nofollow(
    parent: &Dir,
    name: &str,
    expected_metadata: &cap_std::fs::Metadata,
) -> Result<Dir, std::io::Error> {
    let descriptor = rustix::fs::openat(
        parent.as_fd(),
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let opened_metadata = rustix::fs::fstat(&descriptor).map_err(std::io::Error::from)?;
    if i128::from(opened_metadata.st_dev) != i128::from(expected_metadata.dev())
        || i128::from(opened_metadata.st_ino) != i128::from(expected_metadata.ino())
    {
        return Err(std::io::Error::other(
            "Directory changed while Delete was in progress",
        ));
    }
    Ok(Dir::from_std_file(descriptor.into()))
}

#[cfg(not(any(target_os = "android", target_os = "linux", target_vendor = "apple")))]
fn open_directory_nofollow(
    parent: &Dir,
    name: &str,
    _expected_metadata: &cap_std::fs::Metadata,
) -> Result<Dir, std::io::Error> {
    parent.open_dir(name)
}

fn delete_failure(path: &str, operation: &'static str, source: std::io::Error) -> ResourceError {
    ResourceError::DeleteResource {
        path: path.to_owned(),
        operation,
        source,
    }
}

pub(crate) fn open_relative_directory(
    root: &Dir,
    segments: &[ResourceName],
) -> Result<Dir, ResourceError> {
    let mut directory = root.try_clone().map_err(ResourceError::ReadDirectory)?;
    for segment in segments {
        let metadata = directory
            .symlink_metadata(segment.as_str())
            .map_err(map_resolve_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ResourceError::NotDirectory);
        }
        directory = directory
            .open_dir(segment.as_str())
            .map_err(map_resolve_error)?;
    }
    Ok(directory)
}

#[cfg(any(
    target_os = "android",
    target_os = "linux",
    target_os = "redox",
    target_vendor = "apple"
))]
pub(crate) fn rename_noreplace(
    source_directory: &Dir,
    source_name: &str,
    destination_directory: &Dir,
    destination_name: &str,
) -> Result<(), std::io::Error> {
    rustix::fs::renameat_with(
        source_directory.as_fd(),
        source_name,
        destination_directory.as_fd(),
        destination_name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
}

#[cfg(not(any(
    target_os = "android",
    target_os = "linux",
    target_os = "redox",
    target_vendor = "apple"
)))]
pub(crate) fn rename_noreplace(
    source_directory: &Dir,
    source_name: &str,
    destination_directory: &Dir,
    destination_name: &str,
) -> Result<(), std::io::Error> {
    match destination_directory.symlink_metadata(destination_name) {
        Ok(_) => return Err(std::io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    source_directory.rename(source_name, destination_directory, destination_name)
}

fn create_directory_task_error(error: task::JoinError) -> ResourceError {
    ResourceError::CreateDirectory(std::io::Error::other(error))
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
    let resource_path = parse_user_path(config, path)?;
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
        let raw_name = entry
            .file_name()
            .into_string()
            .map_err(|_| ResourceError::InvalidResourceName)?;
        if raw_name == config.staging_directory_name() {
            continue;
        }
        let name = ResourceName::try_from(raw_name.as_str())
            .map_err(|_| ResourceError::InvalidResourceName)?;

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

        let child_path = resource_path
            .join(&name)
            .map_err(|_| ResourceError::InvalidResourcePath)?;
        resources.push(ResourceRow {
            resource_path: child_path.as_str().to_owned(),
            name: raw_name,
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
        breadcrumbs: breadcrumbs(&resource_path),
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
    let resource_path = parse_user_path(config, path)?;
    if resource_path.is_root() {
        return Err(ResourceError::InvalidResourcePath);
    }

    let (file_path, content_length) =
        resolve_file_path(config.storage_root(), &resource_path).await?;
    let content = File::open(file_path)
        .await
        .map_err(ResourceError::ReadFile)?;

    Ok(FileDownload {
        download_name: resource_path
            .resource_name()
            .ok_or(ResourceError::NotFile)?
            .as_str()
            .to_owned(),
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
    let resource_path = parse_user_path(config, path)?;
    if resource_path.is_root() {
        return Err(ResourceError::RootDirectoryArchive);
    }

    let directory_path = resolve_directory_path(config.storage_root(), &resource_path).await?;
    let directory_name = resource_path
        .resource_name()
        .ok_or(ResourceError::NotDirectory)?
        .as_str()
        .to_owned();
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
    let mut pending_directories =
        vec![(config.storage_root().to_path_buf(), ResourcePath::default())];
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

            let raw_name = entry
                .file_name()
                .into_string()
                .map_err(|_| ResourceError::InvalidResourceName)?;
            if raw_name == config.staging_directory_name() {
                continue;
            }
            let name = ResourceName::try_from(raw_name.as_str())
                .map_err(|_| ResourceError::InvalidResourceName)?;

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
            let resource_path = containing_path
                .join(&name)
                .map_err(|_| ResourceError::InvalidResourcePath)?;

            if raw_name.to_lowercase().contains(&query_lowercase) {
                if resources.len() == result_limit {
                    truncated = true;
                    pending_directories.clear();
                    break;
                }

                resources.push(SearchResultRow {
                    resource: ResourceRow {
                        resource_path: resource_path.as_str().to_owned(),
                        name: raw_name,
                        kind,
                        size: (kind == ResourceKind::File).then_some(metadata.len()),
                        modified_time: format_modified_time(
                            metadata.modified().map_err(ResourceError::ModifiedTime)?,
                            config.server().time_zone(),
                        ),
                    },
                    containing_path: containing_path.as_str().to_owned(),
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

fn parse_user_path(config: &AppConfig, raw: &str) -> Result<ResourcePath, ResourceError> {
    config
        .resource_address_policy()
        .parse_path(raw)
        .map_err(|_| ResourceError::InvalidResourcePath)
}

fn parse_user_name(config: &AppConfig, raw: &str) -> Result<ResourceName, ResourceError> {
    config
        .resource_address_policy()
        .parse_name(raw)
        .map_err(|_| ResourceError::InvalidWriteResourceName)
}

fn breadcrumbs(resource_path: &ResourcePath) -> Vec<BreadcrumbSegment> {
    let mut breadcrumbs = Vec::with_capacity(resource_path.segments().len() + 1);
    breadcrumbs.push(BreadcrumbSegment {
        label: "Root Directory".to_owned(),
        path: String::new(),
    });

    let mut path = String::new();
    for segment in resource_path.segments() {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(segment.as_str());
        breadcrumbs.push(BreadcrumbSegment {
            label: segment.as_str().to_owned(),
            path: path.clone(),
        });
    }

    breadcrumbs
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
    resource_path: &ResourcePath,
) -> Result<PathBuf, ResourceError> {
    let mut path = storage_root.to_path_buf();
    for segment in resource_path.segments() {
        path.push(segment.as_str());
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
            ResourceName::try_from(name.as_str())
                .map_err(|_| ResourceError::InvalidResourceName)?;

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
    resource_path: &ResourcePath,
) -> Result<(PathBuf, u64), ResourceError> {
    let Some((file_name, parent_segments)) = resource_path.segments().split_last() else {
        return Err(ResourceError::NotFile);
    };

    let mut path = storage_root.to_path_buf();
    for segment in parent_segments {
        path.push(segment.as_str());
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(map_resolve_error)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(ResourceError::NotDirectory);
        }
    }

    path.push(file_name.as_str());
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

#[cfg(all(
    test,
    any(target_os = "android", target_os = "linux", target_vendor = "apple")
))]
mod tests {
    use std::os::unix::fs::symlink;

    use anyhow::{Context, Result};
    use cap_std::{ambient_authority, fs::Dir};
    use tokio::fs;

    use super::open_directory_nofollow;

    #[tokio::test]
    async fn test_should_reject_directory_replaced_by_symlink_after_metadata_check() -> Result<()> {
        let storage_root = tempfile::tempdir().context("create temporary storage root")?;
        fs::create_dir(storage_root.path().join("target"))
            .await
            .context("create target Directory")?;
        fs::create_dir(storage_root.path().join("victim"))
            .await
            .context("create victim Directory")?;
        fs::write(storage_root.path().join("victim/sentinel.txt"), b"sentinel")
            .await
            .context("write victim sentinel")?;
        let root = Dir::open_ambient_dir(storage_root.path(), ambient_authority())
            .context("open storage root")?;
        let expected_metadata = root
            .symlink_metadata("target")
            .context("read target metadata")?;
        root.remove_dir("target")
            .context("remove original target")?;
        symlink("victim", storage_root.path().join("target"))
            .context("replace target with symlink")?;

        let _error = open_directory_nofollow(&root, "target", &expected_metadata)
            .expect_err("symlink replacement must be rejected");

        assert_eq!(
            fs::read(storage_root.path().join("victim/sentinel.txt"))
                .await
                .context("read victim sentinel")?,
            b"sentinel",
        );
        Ok(())
    }
}
