//! Validated Resource Path and Resource Name domain values.

use thiserror::Error;

const MAX_RESOURCE_NAME_BYTES: usize = 255;
pub(crate) const MAX_RESOURCE_PATH_BYTES: usize = 4096;
const MAX_RESOURCE_PATH_SEGMENTS: usize = 256;

/// A validated Resource Name.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ResourceName(String);

/// Resource Name parsing failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("resource name is invalid")]
pub(crate) struct ResourceNameError;

impl ResourceName {
    /// Borrow the validated Resource Name.
    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for ResourceName {
    type Error = ResourceNameError;

    fn try_from(name: &str) -> Result<Self, Self::Error> {
        if name.is_empty()
            || name.len() > MAX_RESOURCE_NAME_BYTES
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
            || name.contains('\0')
            || name.chars().any(char::is_control)
        {
            return Err(ResourceNameError);
        }

        Ok(Self(name.to_owned()))
    }
}

/// A validated Resource Path relative to the storage root.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct ResourcePath {
    raw: String,
    segments: Vec<ResourceName>,
}

/// Resource Path parsing failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("resource path is invalid")]
pub(crate) struct ResourcePathError;

impl ResourcePath {
    /// Return whether this path represents the Root Directory.
    #[must_use]
    pub(crate) fn is_root(&self) -> bool {
        self.raw.is_empty()
    }

    /// Borrow the normalized Resource Path string.
    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.raw
    }

    /// Borrow the validated Resource Name segments.
    #[must_use]
    pub(crate) fn segments(&self) -> &[ResourceName] {
        &self.segments
    }

    /// Return the final Resource Name, or `None` for the Root Directory.
    #[must_use]
    pub(crate) fn resource_name(&self) -> Option<&ResourceName> {
        self.segments.last()
    }

    /// Return the containing Resource Path, or `None` for the Root Directory.
    #[must_use]
    pub(crate) fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        let raw = self
            .raw
            .rsplit_once('/')
            .map_or_else(String::new, |(parent, _)| parent.to_owned());
        let mut segments = self.segments.clone();
        segments.pop();
        Some(Self { raw, segments })
    }

    /// Append a Resource Name while preserving Resource Path limits.
    pub(crate) fn join(&self, name: &ResourceName) -> Result<Self, ResourcePathError> {
        let raw = if self.raw.is_empty() {
            name.as_str().to_owned()
        } else {
            format!("{}/{name}", self.raw, name = name.as_str())
        };
        Self::try_from(raw.as_str())
    }
}

impl TryFrom<&str> for ResourcePath {
    type Error = ResourcePathError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        if raw.len() > MAX_RESOURCE_PATH_BYTES {
            return Err(ResourcePathError);
        }
        if raw.is_empty() {
            return Ok(Self {
                raw: String::new(),
                segments: Vec::new(),
            });
        }

        let segments = raw
            .split('/')
            .map(ResourceName::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ResourcePathError)?;
        if segments.len() > MAX_RESOURCE_PATH_SEGMENTS {
            return Err(ResourcePathError);
        }

        Ok(Self {
            raw: raw.to_owned(),
            segments,
        })
    }
}

/// Hub policy for Resource addresses reserved for internal storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceAddressPolicy {
    reserved_name: ResourceName,
}

impl ResourceAddressPolicy {
    /// Create a policy with the configured reserved staging name.
    pub(crate) const fn new(reserved_name: ResourceName) -> Self {
        Self { reserved_name }
    }

    /// Parse a user-supplied Resource Name and reject the reserved name.
    pub(crate) fn parse_name(&self, raw: &str) -> Result<ResourceName, ResourceNameError> {
        let name = ResourceName::try_from(raw)?;
        if name == self.reserved_name {
            return Err(ResourceNameError);
        }
        Ok(name)
    }

    /// Parse a user-supplied Resource Path and reject any reserved segment.
    pub(crate) fn parse_path(&self, raw: &str) -> Result<ResourcePath, ResourcePathError> {
        let path = ResourcePath::try_from(raw)?;
        if path.segments.contains(&self.reserved_name) {
            return Err(ResourcePathError);
        }
        Ok(path)
    }

    /// Borrow the configured reserved staging name.
    #[must_use]
    pub(crate) const fn reserved_name(&self) -> &ResourceName {
        &self.reserved_name
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{ResourceAddressPolicy, ResourceName, ResourcePath};

    #[test]
    fn test_should_parse_empty_path_as_root_directory() {
        let path = ResourcePath::try_from("").expect("Root Directory path should be valid");

        assert!(path.is_root());
        assert_eq!(path.as_str(), "");
    }

    #[test]
    fn test_should_reject_invalid_resource_names() {
        for invalid_name in ["", ".", "..", "a/b", "a\\b", "a\0b", "a\nb"] {
            assert!(ResourceName::try_from(invalid_name).is_err());
        }
    }

    #[test]
    fn test_should_accept_human_readable_and_leading_dot_resource_names() {
        for valid_name in ["资料 2026.txt", ".gitignore", "résumé.pdf", "照片.jpg"] {
            let name = ResourceName::try_from(valid_name)
                .expect("ordinary human-readable Resource Name should be valid");
            assert_eq!(name.as_str(), valid_name);
        }
    }

    #[test]
    fn test_should_reject_resource_paths_with_invalid_segments() {
        for invalid_path in [
            "/docs",
            "docs/",
            "docs//guide",
            "docs/../secret",
            "docs\\guide",
        ] {
            assert!(ResourcePath::try_from(invalid_path).is_err());
        }
    }

    #[test]
    fn test_should_reject_reserved_name_from_user_addresses() {
        let reserved_name = ResourceName::try_from(".fh-staging")
            .expect("reserved staging name should be lexically valid");
        let policy = ResourceAddressPolicy::new(reserved_name);

        assert!(policy.parse_name(".fh-staging").is_err());
        assert!(policy.parse_path("docs/.fh-staging/file.txt").is_err());
        assert!(policy.parse_path("docs/file.txt").is_ok());
    }

    #[test]
    fn test_should_join_resource_name_into_valid_resource_path() {
        let root = ResourcePath::try_from("").expect("Root Directory path should be valid");
        let docs = ResourceName::try_from("docs").expect("Resource Name should be valid");
        let guide = ResourceName::try_from("guide.txt").expect("Resource Name should be valid");

        let docs_path = root.join(&docs).expect("joined path should be valid");
        let guide_path = docs_path.join(&guide).expect("joined path should be valid");

        assert_eq!(docs_path.as_str(), "docs");
        assert_eq!(guide_path.as_str(), "docs/guide.txt");
        assert_eq!(guide_path.resource_name(), Some(&guide));
        assert_eq!(guide_path.parent(), Some(docs_path));
    }

    #[test]
    fn test_should_enforce_resource_name_byte_limit() {
        assert!(ResourceName::try_from("a".repeat(255).as_str()).is_ok());
        assert!(ResourceName::try_from("a".repeat(256).as_str()).is_err());
    }

    #[test]
    fn test_should_enforce_resource_path_byte_and_segment_limits() {
        let maximum_path = std::iter::once("a".repeat(16))
            .chain(std::iter::repeat_n("a".repeat(15), 255))
            .collect::<Vec<_>>()
            .join("/");
        let oversized_path = format!("a{maximum_path}");
        let too_many_segments = std::iter::repeat_n("a", 257).collect::<Vec<_>>().join("/");

        assert_eq!(maximum_path.len(), 4096);
        assert!(ResourcePath::try_from(maximum_path.as_str()).is_ok());
        assert!(ResourcePath::try_from(oversized_path.as_str()).is_err());
        assert!(ResourcePath::try_from(too_many_segments.as_str()).is_err());
    }

    proptest! {
        #[test]
        fn test_should_round_trip_joined_resource_paths(
            segments in prop::collection::vec("[A-Za-z0-9_-]{1,16}", 0..32),
        ) {
            let mut path = ResourcePath::default();
            for segment in &segments {
                let name = ResourceName::try_from(segment.as_str())?;
                path = path.join(&name)?;
            }

            let reparsed = ResourcePath::try_from(path.as_str())?;
            let reparsed_segments = reparsed
                .segments()
                .iter()
                .map(ResourceName::as_str)
                .collect::<Vec<_>>();

            prop_assert_eq!(reparsed_segments, segments);
        }
    }
}
