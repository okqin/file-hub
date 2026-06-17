//! Storage-root-backed resource listing.

use std::{cmp::Ordering, path::PathBuf};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{fs, fs::File};

use crate::config::AppConfig;

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

/// Resource listing failure.
#[derive(Debug, Error)]
pub enum ResourceError {
    /// The requested resource path is invalid.
    #[error("resource path is invalid")]
    InvalidResourcePath,
    /// The Current List Filter query is invalid.
    #[error("current list filter query is invalid")]
    InvalidFilter,
    /// The requested resource path is not a directory resource.
    #[error("resource path is not a directory")]
    NotDirectory,
    /// The requested resource path is not a file resource.
    #[error("resource path is not a file")]
    NotFile,
    /// The requested resource path does not exist.
    #[error("resource path does not exist")]
    ResourceNotFound,
    /// The configured directory contains more resources than the direct child limit.
    #[error("direct child listing exceeds configured limit of {limit}")]
    ListingLimitExceeded {
        /// Configured direct child resource limit.
        limit: usize,
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

#[derive(Debug)]
struct ResourcePath<'a> {
    raw: &'a str,
    segments: Vec<&'a str>,
}

impl<'a> ResourcePath<'a> {
    fn parse(raw: &'a str) -> Result<Self, ResourceError> {
        if raw.is_empty() {
            return Ok(Self {
                raw,
                segments: Vec::new(),
            });
        }

        let segments: Vec<&str> = raw.split('/').collect();
        if segments
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

fn is_valid_resource_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
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
