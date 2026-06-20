//! Atomic File and Directory Upload transactions.

use std::{collections::HashSet, error::Error, fmt::Write as FmtWrite};

use bytes::Bytes;
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions as CapOpenOptions},
};
use futures_util::{Stream, StreamExt};
use thiserror::Error;
use tokio::{fs::File, io::AsyncWriteExt, task};
use tracing::warn;

use crate::{
    config::AppConfig,
    resource::{
        ResourceError, ResourcePath, is_valid_resource_name, open_relative_directory,
        owned_segments, rename_noreplace,
    },
};

type BoxError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Error)]
pub enum UploadError {
    #[error("upload input is invalid")]
    InvalidInput,
    #[error("upload input stream failed")]
    InputRead(#[source] BoxError),
    #[error("resource path is invalid")]
    InvalidResourcePath,
    #[error("resource path is not a directory")]
    NotDirectory,
    #[error("resource path does not exist")]
    ResourceNotFound,
    #[error("resource name is invalid")]
    InvalidWriteResourceName,
    #[error("relative path contains an invalid Resource Name")]
    InvalidDirectoryUploadPath { path: String },
    #[error("Directory Upload conflicts with an existing Resource")]
    DirectoryUploadConflict { path: String },
    #[error("Directory Upload file exceeds configured size limit of {limit} bytes")]
    DirectoryUploadSingleFileSizeLimitExceeded { path: String, limit: u64 },
    #[error("Directory Upload exceeds configured total size limit of {limit} bytes")]
    DirectoryUploadTotalSizeLimitExceeded { path: String, limit: u64 },
    #[error("Directory Upload exceeds configured Resource count limit of {limit}")]
    DirectoryUploadResourceCountLimitExceeded { path: String, limit: usize },
    #[error("resource name conflicts with an existing resource")]
    NameConflict,
    #[error("uploaded file exceeds configured size limit of {limit} bytes")]
    UploadSingleFileSizeLimitExceeded { limit: u64 },
    #[error("upload exceeds configured total size limit of {limit} bytes")]
    UploadTotalSizeLimitExceeded { limit: u64 },
    #[error("failed to store uploaded resource")]
    Store(#[source] std::io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UploadKind {
    File,
    Directory,
}

pub(crate) trait UploadInput {
    async fn consume(self, receiver: &mut UploadReceiver<'_>) -> Result<(), UploadError>;
}

#[derive(Debug)]
pub(crate) struct UploadReceiver<'a> {
    config: &'a AppConfig,
    transaction: Option<Transaction>,
}

#[derive(Debug)]
enum Transaction {
    File {
        current_path: String,
        staged: Option<StagedFileUpload>,
    },
    Directory {
        staged: StagedDirectoryUpload,
        file_count: usize,
    },
}

#[derive(Debug)]
struct StagedFileUpload {
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

#[derive(Debug)]
struct StagedDirectoryUpload {
    staging_directory: Dir,
    staged_tree: Dir,
    destination_directory: Dir,
    staging_name: String,
    reserved_name: String,
    top_level_name: Option<String>,
    active_file: Option<File>,
    active_relative_path: Option<String>,
    cleanup_staging: bool,
    active_file_bytes: u64,
    total_bytes: u64,
    single_file_limit: u64,
    total_upload_limit: u64,
    resource_paths: HashSet<String>,
    resource_count_limit: usize,
}

pub(crate) async fn execute<I>(config: &AppConfig, input: I) -> Result<(), UploadError>
where
    I: UploadInput,
{
    let mut receiver = UploadReceiver {
        config,
        transaction: None,
    };
    if let Err(error) = input.consume(&mut receiver).await {
        receiver.abort().await;
        return Err(error);
    }
    receiver.commit().await
}

pub(crate) async fn cleanup_staging_remnants(config: &AppConfig) -> Result<(), UploadError> {
    let storage_root = config.storage_root().to_path_buf();
    let staging_directory_name = config.staging_directory_name().to_owned();
    let staging_path = storage_root.join(&staging_directory_name);
    let preserved_database_name = if config.database_path().parent() == Some(staging_path.as_path())
    {
        config
            .database_path()
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    } else {
        None
    };
    task::spawn_blocking(move || {
        let root =
            Dir::open_ambient_dir(storage_root, ambient_authority()).map_err(UploadError::Store)?;
        let metadata = match root.symlink_metadata(&staging_directory_name) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(UploadError::Store(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(UploadError::Store(std::io::Error::other(
                "reserved staging path is not a directory",
            )));
        }
        let staging = root
            .open_dir(&staging_directory_name)
            .map_err(UploadError::Store)?;
        let entries = staging.entries().map_err(UploadError::Store)?;
        for entry in entries {
            let entry = entry.map_err(UploadError::Store)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                UploadError::Store(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "staging entry name is not valid UTF-8",
                ))
            })?;
            if preserved_database_name.as_deref().is_some_and(|database| {
                name == database
                    || name == format!("{database}-wal")
                    || name == format!("{database}-shm")
                    || name == format!("{database}-journal")
            }) {
                continue;
            }
            let metadata = staging.symlink_metadata(name).map_err(UploadError::Store)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                staging.remove_dir_all(name).map_err(UploadError::Store)?;
            } else {
                staging.remove_file(name).map_err(UploadError::Store)?;
            }
        }
        Ok(())
    })
    .await
    .map_err(blocking_task_error)??;
    Ok(())
}

impl UploadReceiver<'_> {
    pub(crate) async fn begin(
        &mut self,
        current_path: &str,
        kind: UploadKind,
    ) -> Result<(), UploadError> {
        if self.transaction.is_some() {
            return Err(UploadError::InvalidInput);
        }
        self.transaction = Some(match kind {
            UploadKind::File => Transaction::File {
                current_path: current_path.to_owned(),
                staged: None,
            },
            UploadKind::Directory => Transaction::Directory {
                staged: StagedDirectoryUpload::start(self.config, current_path).await?,
                file_count: 0,
            },
        });
        Ok(())
    }

    pub(crate) async fn receive_file<S, E>(
        &mut self,
        path: &str,
        mut chunks: S,
    ) -> Result<(), UploadError>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: Error + Send + Sync + 'static,
    {
        match self.transaction.as_mut().ok_or(UploadError::InvalidInput)? {
            Transaction::File {
                current_path,
                staged,
            } => {
                if staged.is_some() {
                    return Err(UploadError::InvalidInput);
                }
                let mut upload = StagedFileUpload::start(self.config, current_path, path).await?;
                while let Some(chunk) = chunks.next().await {
                    upload
                        .write_chunk(
                            &chunk.map_err(|error| UploadError::InputRead(Box::new(error)))?,
                        )
                        .await?;
                }
                *staged = Some(upload);
            }
            Transaction::Directory { staged, file_count } => {
                staged.start_file(path).await?;
                while let Some(chunk) = chunks.next().await {
                    staged
                        .write_chunk(
                            &chunk.map_err(|error| UploadError::InputRead(Box::new(error)))?,
                        )
                        .await?;
                }
                staged.finish_file().await?;
                *file_count = file_count.checked_add(1).ok_or(UploadError::InvalidInput)?;
            }
        }
        Ok(())
    }

    async fn commit(mut self) -> Result<(), UploadError> {
        match self.transaction.take().ok_or(UploadError::InvalidInput)? {
            Transaction::File {
                staged: Some(upload),
                ..
            } => upload.commit().await,
            Transaction::File { staged: None, .. }
            | Transaction::Directory { file_count: 0, .. } => Err(UploadError::InvalidInput),
            Transaction::Directory { staged, .. } => staged.commit().await,
        }
    }

    async fn abort(&mut self) {
        match self.transaction.take() {
            Some(Transaction::File {
                staged: Some(upload),
                ..
            }) => upload.abort().await,
            Some(Transaction::Directory { staged, .. }) => staged.abort().await,
            Some(Transaction::File { staged: None, .. }) | None => {}
        }
    }
}

impl StagedFileUpload {
    async fn start(config: &AppConfig, path: &str, name: &str) -> Result<Self, UploadError> {
        let resource_path = ResourcePath::parse(path).map_err(UploadError::from_resource)?;
        if resource_path.contains_reserved_name(config.staging_directory_name()) {
            return Err(UploadError::InvalidResourcePath);
        }
        if !is_valid_resource_name(name) || name == config.staging_directory_name() {
            return Err(UploadError::InvalidWriteResourceName);
        }

        let storage_root = config.storage_root().to_path_buf();
        let segments = owned_segments(&resource_path);
        let destination_name = name.to_owned();
        let staging_directory_name = config.staging_directory_name().to_owned();
        let (file, staging_directory, destination_directory, staging_name) =
            task::spawn_blocking(move || {
                let root = Dir::open_ambient_dir(storage_root, ambient_authority())
                    .map_err(UploadError::Store)?;
                let destination_directory = open_relative_directory(&root, &segments)
                    .map_err(UploadError::from_resource)?;
                match destination_directory.symlink_metadata(&destination_name) {
                    Ok(_) => return Err(UploadError::NameConflict),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(UploadError::Store(error)),
                }
                let staging_directory = open_staging_directory(&root, &staging_directory_name)?;
                let staging_name = create_staging_name()?;
                let mut options = CapOpenOptions::new();
                options.write(true).create_new(true);
                let file = staging_directory
                    .open_with(&staging_name, &options)
                    .map_err(UploadError::Store)?;
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

    async fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), UploadError> {
        let chunk_length = u64::try_from(chunk.len()).map_err(|_| {
            UploadError::UploadSingleFileSizeLimitExceeded {
                limit: self.single_file_limit,
            }
        })?;
        let next_length = self.bytes_written.checked_add(chunk_length).ok_or(
            UploadError::UploadSingleFileSizeLimitExceeded {
                limit: self.single_file_limit,
            },
        )?;
        if next_length > self.single_file_limit {
            return Err(UploadError::UploadSingleFileSizeLimitExceeded {
                limit: self.single_file_limit,
            });
        }
        if next_length > self.total_upload_limit {
            return Err(UploadError::UploadTotalSizeLimitExceeded {
                limit: self.total_upload_limit,
            });
        }
        self.file
            .write_all(chunk)
            .await
            .map_err(UploadError::Store)?;
        self.bytes_written = next_length;
        Ok(())
    }

    async fn commit(mut self) -> Result<(), UploadError> {
        self.file.sync_all().await.map_err(UploadError::Store)?;
        let staging_directory = self
            .staging_directory
            .try_clone()
            .map_err(UploadError::Store)?;
        let destination_directory = self
            .destination_directory
            .try_clone()
            .map_err(UploadError::Store)?;
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
                Err(UploadError::NameConflict)
            }
            Err(error) => Err(UploadError::Store(error)),
        }
    }

    async fn abort(mut self) {
        if let Err(error) = self.remove_staging().await {
            warn!(%error, staging_name = %self.staging_name, "failed to abort staged upload");
        }
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
        if self.cleanup_staging
            && let Err(error) = self.staging_directory.remove_file(&self.staging_name)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(%error, staging_name = %self.staging_name, "failed to clean up staged upload");
        }
    }
}

impl StagedDirectoryUpload {
    async fn start(config: &AppConfig, path: &str) -> Result<Self, UploadError> {
        let resource_path = ResourcePath::parse(path).map_err(UploadError::from_resource)?;
        if resource_path.contains_reserved_name(config.staging_directory_name()) {
            return Err(UploadError::InvalidResourcePath);
        }

        let storage_root = config.storage_root().to_path_buf();
        let segments = owned_segments(&resource_path);
        let staging_directory_name = config.staging_directory_name().to_owned();
        let (staging_directory, staged_tree, destination_directory, staging_name) =
            task::spawn_blocking(move || {
                let root = Dir::open_ambient_dir(storage_root, ambient_authority())
                    .map_err(UploadError::Store)?;
                let destination_directory = open_relative_directory(&root, &segments)
                    .map_err(UploadError::from_resource)?;
                let staging_directory = open_staging_directory(&root, &staging_directory_name)?;
                let staging_name = create_staging_name()?;
                staging_directory
                    .create_dir(&staging_name)
                    .map_err(UploadError::Store)?;
                let staged_tree = staging_directory
                    .open_dir(&staging_name)
                    .map_err(UploadError::Store)?;
                Ok((
                    staging_directory,
                    staged_tree,
                    destination_directory,
                    staging_name,
                ))
            })
            .await
            .map_err(blocking_task_error)??;

        Ok(Self {
            staging_directory,
            staged_tree,
            destination_directory,
            staging_name,
            reserved_name: config.staging_directory_name().to_owned(),
            top_level_name: None,
            active_file: None,
            active_relative_path: None,
            cleanup_staging: true,
            active_file_bytes: 0,
            total_bytes: 0,
            single_file_limit: config.limits().upload_single_file_size_limit_bytes().get(),
            total_upload_limit: config.limits().upload_total_size_limit_bytes().get(),
            resource_paths: HashSet::new(),
            resource_count_limit: config
                .limits()
                .directory_upload_resource_count_limit()
                .get(),
        })
    }

    async fn start_file(&mut self, relative_path: &str) -> Result<(), UploadError> {
        if self.active_file.is_some() {
            return Err(UploadError::InvalidInput);
        }
        let nested_segments = self.validate_relative_path(relative_path)?;
        let file = self
            .create_staged_file(nested_segments, relative_path.to_owned())
            .await?;
        self.active_file = Some(file);
        self.active_relative_path = Some(relative_path.to_owned());
        self.active_file_bytes = 0;
        Ok(())
    }

    fn validate_relative_path(&mut self, relative_path: &str) -> Result<Vec<String>, UploadError> {
        let resource_path = ResourcePath::parse(relative_path).map_err(|_| {
            UploadError::InvalidDirectoryUploadPath {
                path: relative_path.to_owned(),
            }
        })?;
        if resource_path.contains_reserved_name(&self.reserved_name) {
            return Err(UploadError::InvalidDirectoryUploadPath {
                path: relative_path.to_owned(),
            });
        }
        let Some((top_level_name, nested_segments)) = resource_path.segments.split_first() else {
            return Err(UploadError::InvalidDirectoryUploadPath {
                path: relative_path.to_owned(),
            });
        };
        if nested_segments.is_empty() {
            return Err(UploadError::InvalidDirectoryUploadPath {
                path: relative_path.to_owned(),
            });
        }
        match self.top_level_name.as_deref() {
            Some(expected) if expected != *top_level_name => {
                return Err(UploadError::InvalidDirectoryUploadPath {
                    path: relative_path.to_owned(),
                });
            }
            None => self.top_level_name = Some((*top_level_name).to_owned()),
            Some(_) => {}
        }
        let mut cumulative_path = String::new();
        for segment in &resource_path.segments {
            if !cumulative_path.is_empty() {
                cumulative_path.push('/');
            }
            cumulative_path.push_str(segment);
            if self.resource_paths.insert(cumulative_path.clone())
                && self.resource_paths.len() > self.resource_count_limit
            {
                return Err(UploadError::DirectoryUploadResourceCountLimitExceeded {
                    path: relative_path.to_owned(),
                    limit: self.resource_count_limit,
                });
            }
        }
        Ok(nested_segments
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect())
    }

    async fn create_staged_file(
        &self,
        nested_segments: Vec<String>,
        failure_path: String,
    ) -> Result<File, UploadError> {
        let staged_tree = self.staged_tree.try_clone().map_err(UploadError::Store)?;
        task::spawn_blocking(move || {
            let Some((file_name, parent_segments)) = nested_segments.split_last() else {
                return Err(UploadError::InvalidResourcePath);
            };
            let mut parent = staged_tree;
            for segment in parent_segments {
                match parent.create_dir(segment) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = parent
                            .symlink_metadata(segment)
                            .map_err(UploadError::Store)?;
                        if !metadata.is_dir() || metadata.file_type().is_symlink() {
                            return Err(UploadError::DirectoryUploadConflict {
                                path: failure_path.clone(),
                            });
                        }
                    }
                    Err(error) => return Err(UploadError::Store(error)),
                }
                parent = parent.open_dir(segment).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::NotADirectory {
                        UploadError::DirectoryUploadConflict {
                            path: failure_path.clone(),
                        }
                    } else {
                        UploadError::Store(error)
                    }
                })?;
            }
            let mut options = CapOpenOptions::new();
            options.write(true).create_new(true);
            parent
                .open_with(file_name, &options)
                .map(|file| File::from_std(file.into_std()))
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        UploadError::DirectoryUploadConflict { path: failure_path }
                    } else {
                        UploadError::Store(error)
                    }
                })
        })
        .await
        .map_err(blocking_task_error)?
    }

    async fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), UploadError> {
        let active_relative_path = self
            .active_relative_path
            .as_deref()
            .ok_or(UploadError::InvalidInput)?;
        let chunk_length = u64::try_from(chunk.len()).map_err(|_| {
            UploadError::DirectoryUploadSingleFileSizeLimitExceeded {
                path: active_relative_path.to_owned(),
                limit: self.single_file_limit,
            }
        })?;
        let next_file_bytes = self
            .active_file_bytes
            .checked_add(chunk_length)
            .ok_or_else(|| UploadError::DirectoryUploadSingleFileSizeLimitExceeded {
                path: active_relative_path.to_owned(),
                limit: self.single_file_limit,
            })?;
        if next_file_bytes > self.single_file_limit {
            return Err(UploadError::DirectoryUploadSingleFileSizeLimitExceeded {
                path: active_relative_path.to_owned(),
                limit: self.single_file_limit,
            });
        }
        let next_total_bytes = self.total_bytes.checked_add(chunk_length).ok_or_else(|| {
            UploadError::DirectoryUploadTotalSizeLimitExceeded {
                path: active_relative_path.to_owned(),
                limit: self.total_upload_limit,
            }
        })?;
        if next_total_bytes > self.total_upload_limit {
            return Err(UploadError::DirectoryUploadTotalSizeLimitExceeded {
                path: active_relative_path.to_owned(),
                limit: self.total_upload_limit,
            });
        }
        self.active_file
            .as_mut()
            .ok_or(UploadError::InvalidInput)?
            .write_all(chunk)
            .await
            .map_err(UploadError::Store)?;
        self.active_file_bytes = next_file_bytes;
        self.total_bytes = next_total_bytes;
        Ok(())
    }

    async fn finish_file(&mut self) -> Result<(), UploadError> {
        let file = self.active_file.take().ok_or(UploadError::InvalidInput)?;
        self.active_relative_path = None;
        file.sync_all().await.map_err(UploadError::Store)
    }

    async fn commit(mut self) -> Result<(), UploadError> {
        if self.active_file.is_some() {
            return Err(UploadError::InvalidInput);
        }
        let destination_name = self
            .top_level_name
            .clone()
            .ok_or(UploadError::InvalidInput)?;
        let conflict_path = destination_name.clone();
        let staging_directory = self
            .staging_directory
            .try_clone()
            .map_err(UploadError::Store)?;
        let destination_directory = self
            .destination_directory
            .try_clone()
            .map_err(UploadError::Store)?;
        let staging_name = self.staging_name.clone();
        task::spawn_blocking(move || {
            rename_noreplace(
                &staging_directory,
                &staging_name,
                &destination_directory,
                &destination_name,
            )
            .map_err(|error| {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
                ) {
                    UploadError::DirectoryUploadConflict {
                        path: conflict_path,
                    }
                } else {
                    UploadError::Store(error)
                }
            })
        })
        .await
        .map_err(blocking_task_error)??;
        self.cleanup_staging = false;
        Ok(())
    }

    async fn abort(mut self) {
        drop(self.active_file.take());
        self.active_relative_path = None;
        let Ok(staging_directory) = self.staging_directory.try_clone() else {
            return;
        };
        let staging_name = self.staging_name.clone();
        match task::spawn_blocking(move || staging_directory.remove_dir_all(staging_name)).await {
            Ok(Ok(())) => self.cleanup_staging = false,
            Ok(Err(error)) => {
                warn!(%error, staging_name = %self.staging_name, "failed to abort staged Directory Upload");
            }
            Err(error) => {
                warn!(%error, staging_name = %self.staging_name, "staging cleanup task failed");
            }
        }
    }
}

impl Drop for StagedDirectoryUpload {
    fn drop(&mut self) {
        drop(self.active_file.take());
        if self.cleanup_staging
            && let Err(error) = self.staging_directory.remove_dir_all(&self.staging_name)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(%error, staging_name = %self.staging_name, "failed to clean up staged Directory Upload");
        }
    }
}

impl UploadError {
    fn from_resource(error: ResourceError) -> Self {
        match error {
            ResourceError::InvalidResourcePath => Self::InvalidResourcePath,
            ResourceError::NotDirectory => Self::NotDirectory,
            ResourceError::ResourceNotFound => Self::ResourceNotFound,
            ResourceError::ReadDirectory(source) => Self::Store(source),
            error => Self::Store(std::io::Error::other(error.to_string())),
        }
    }
}

fn open_staging_directory(root: &Dir, name: &str) -> Result<Dir, UploadError> {
    match root.create_dir(name) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(UploadError::Store(error)),
    }
    let metadata = root.symlink_metadata(name).map_err(UploadError::Store)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(UploadError::Store(std::io::Error::other(
            "reserved staging path is not a directory",
        )));
    }
    root.open_dir(name).map_err(UploadError::Store)
}

fn create_staging_name() -> Result<String, UploadError> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|error| UploadError::Store(std::io::Error::other(error.to_string())))?;
    let mut name = String::with_capacity(random.len() * 2);
    for byte in random {
        write!(&mut name, "{byte:02x}")
            .map_err(|error| UploadError::Store(std::io::Error::other(error.to_string())))?;
    }
    Ok(name)
}

fn blocking_task_error(error: task::JoinError) -> UploadError {
    UploadError::Store(std::io::Error::other(error))
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result};
    use bytes::Bytes;
    use futures_util::stream;
    use tempfile::TempDir;
    use tokio::fs;

    use super::{UploadInput, UploadKind, UploadReceiver, execute};
    use crate::config::AppConfig;

    #[derive(Debug)]
    struct MemoryFileInput;

    #[derive(Debug)]
    struct FailingDirectoryInput;

    #[derive(Debug)]
    struct MemoryDirectoryInput;

    impl UploadInput for MemoryFileInput {
        async fn consume(
            self,
            receiver: &mut UploadReceiver<'_>,
        ) -> Result<(), super::UploadError> {
            receiver.begin("", UploadKind::File).await?;
            receiver
                .receive_file(
                    "hello.txt",
                    stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"hello"))]),
                )
                .await
        }
    }

    impl UploadInput for FailingDirectoryInput {
        async fn consume(
            self,
            receiver: &mut UploadReceiver<'_>,
        ) -> Result<(), super::UploadError> {
            receiver.begin("", UploadKind::Directory).await?;
            receiver
                .receive_file(
                    "selected/file.txt",
                    stream::iter([
                        Ok(Bytes::from_static(b"partial")),
                        Err(std::io::Error::other("injected stream failure")),
                    ]),
                )
                .await
        }
    }

    impl UploadInput for MemoryDirectoryInput {
        async fn consume(
            self,
            receiver: &mut UploadReceiver<'_>,
        ) -> Result<(), super::UploadError> {
            receiver.begin("", UploadKind::Directory).await?;
            receiver
                .receive_file(
                    "selected/docs/guide.txt",
                    stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(b"guide"))]),
                )
                .await
        }
    }

    #[tokio::test]
    async fn test_should_publish_file_through_upload_interface() -> Result<()> {
        let storage_root = tempfile::tempdir().context("create temporary storage root")?;
        let (config, _config_dir) = test_config(&storage_root).await?;

        execute(&config, MemoryFileInput).await?;

        assert_eq!(
            fs::read(storage_root.path().join("hello.txt")).await?,
            b"hello"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_should_roll_back_when_input_stream_fails() -> Result<()> {
        let storage_root = tempfile::tempdir().context("create temporary storage root")?;
        let (config, _config_dir) = test_config(&storage_root).await?;

        let result = execute(&config, FailingDirectoryInput).await;

        assert!(matches!(result, Err(super::UploadError::InputRead(_))));
        assert!(!storage_root.path().join("selected").exists());
        let mut staging = fs::read_dir(storage_root.path().join(".fh-staging")).await?;
        assert!(staging.next_entry().await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_should_publish_directory_through_upload_interface() -> Result<()> {
        let storage_root = tempfile::tempdir().context("create temporary storage root")?;
        let (config, _config_dir) = test_config(&storage_root).await?;

        execute(&config, MemoryDirectoryInput).await?;

        assert_eq!(
            fs::read(storage_root.path().join("selected/docs/guide.txt")).await?,
            b"guide",
        );
        Ok(())
    }

    async fn test_config(storage_root: &TempDir) -> Result<(AppConfig, TempDir)> {
        let config_dir = tempfile::tempdir().context("create temporary config directory")?;
        let config_path = config_dir.path().join("file-hub.yaml");
        let config_text = format!(
            r#"
storage_root: {storage_root:?}
staging_directory_name: ".fh-staging"
server:
  bind_address: "127.0.0.1:0"
  time_zone: "UTC"
limits:
  upload_single_file_size_limit_bytes: 1024
  upload_total_size_limit_bytes: 4096
  directory_upload_resource_count_limit: 100
  listing_direct_child_limit: 100
  archive_resource_count_limit: 100
  archive_uncompressed_size_limit_bytes: 4096
  search_result_limit: 100
  search_traversal_limit: 1000
  request_body_limit_bytes: 8192
  request_timeout_seconds: 5
  request_concurrency_limit: 16
  fs_concurrency_limit: 4
"#,
            storage_root = storage_root.path().to_string_lossy(),
        );
        fs::write(&config_path, config_text)
            .await
            .context("write test config")?;
        Ok((AppConfig::load_from_path(&config_path).await?, config_dir))
    }
}
