//! Storage-root-backed resource listing.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use thiserror::Error;
use tokio::fs;

use crate::config::AppConfig;

/// Root directory listing response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    /// The current resource path. The Root Directory is represented by an empty path.
    pub path: String,
    /// Direct child resources under the current directory.
    pub resources: Vec<ResourceRow>,
}

/// A browser-visible resource row.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRow {
    /// Resource name within the Root Directory.
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

/// Resource listing failure.
#[derive(Debug, Error)]
pub enum ResourceError {
    /// The configured directory contains more resources than the direct child limit.
    #[error("direct child listing exceeds configured limit of {limit}")]
    ListingLimitExceeded {
        /// Configured direct child resource limit.
        limit: usize,
    },
    /// The storage root could not be read.
    #[error("failed to read root directory")]
    ReadRoot(#[source] std::io::Error),
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
}

/// List direct resources in the Root Directory.
///
/// # Errors
///
/// Returns an error when the storage root cannot be read, resource metadata is unavailable,
/// resource names are invalid, or the configured listing limit is exceeded.
pub async fn list_root_directory(config: &AppConfig) -> Result<DirectoryListing, ResourceError> {
    let mut read_dir = fs::read_dir(config.storage_root())
        .await
        .map_err(ResourceError::ReadRoot)?;
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

        let metadata = entry.metadata().await.map_err(ResourceError::Metadata)?;
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

        resources.push(ResourceRow {
            resource_path: name.clone(),
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

    resources.sort_by(compare_resource_rows);

    Ok(DirectoryListing {
        path: String::new(),
        resources,
    })
}

fn compare_resource_rows(left: &ResourceRow, right: &ResourceRow) -> Ordering {
    resource_kind_rank(left.kind)
        .cmp(&resource_kind_rank(right.kind))
        .then_with(|| left.name.cmp(&right.name))
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
