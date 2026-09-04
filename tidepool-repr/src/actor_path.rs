//! Validated, human-readable actor lineage paths shared by actor and worktree owners.

use std::fmt;

/// Maximum bytes in one model-authored lineage segment.
pub const MAX_ACTOR_PATH_SEGMENT_BYTES: usize = 48;
/// Maximum bytes in a complete slash-separated lineage path.
pub const MAX_ACTOR_PATH_BYTES: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActorPathSegment(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActorPathError {
    #[error("actor path segment is empty")]
    EmptySegment,
    #[error("actor path segment `{segment}` exceeds {maximum} bytes")]
    SegmentTooLong { segment: String, maximum: usize },
    #[error("actor path segment `{segment}` must be lowercase kebab-case")]
    InvalidSegment { segment: String },
    #[error("actor path must contain at least one segment")]
    EmptyPath,
    #[error("actor path `{path}` exceeds {maximum} bytes")]
    PathTooLong { path: String, maximum: usize },
}

impl ActorPathSegment {
    pub fn new(value: impl Into<String>) -> Result<Self, ActorPathError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ActorPathError::EmptySegment);
        }
        if value.len() > MAX_ACTOR_PATH_SEGMENT_BYTES {
            return Err(ActorPathError::SegmentTooLong {
                segment: value,
                maximum: MAX_ACTOR_PATH_SEGMENT_BYTES,
            });
        }
        let bytes = value.as_bytes();
        let valid = bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
            && !value.contains("--");
        if !valid {
            return Err(ActorPathError::InvalidSegment { segment: value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn numbered(&self, ordinal: usize) -> Result<Self, ActorPathError> {
        Self::new(format!("{}-{ordinal}", self.0))
    }
}

impl fmt::Display for ActorPathSegment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActorPath(Vec<ActorPathSegment>);

impl ActorPath {
    pub fn new(segments: Vec<ActorPathSegment>) -> Result<Self, ActorPathError> {
        if segments.is_empty() {
            return Err(ActorPathError::EmptyPath);
        }
        let path = segments
            .iter()
            .map(ActorPathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/");
        if path.len() > MAX_ACTOR_PATH_BYTES {
            return Err(ActorPathError::PathTooLong {
                path,
                maximum: MAX_ACTOR_PATH_BYTES,
            });
        }
        Ok(Self(segments))
    }

    pub fn parse(path: &str) -> Result<Self, ActorPathError> {
        if path.is_empty() {
            return Err(ActorPathError::EmptyPath);
        }
        Self::new(
            path.split('/')
                .map(|segment| ActorPathSegment::new(segment.to_owned()))
                .collect::<Result<Vec<_>, _>>()?,
        )
    }

    pub fn child(&self, segment: ActorPathSegment) -> Result<Self, ActorPathError> {
        let mut segments = self.0.clone();
        segments.push(segment);
        Self::new(segments)
    }

    #[must_use]
    pub fn segments(&self) -> &[ActorPathSegment] {
        &self.0
    }

    #[must_use]
    pub fn git_branch(&self) -> String {
        format!("shoal/{self}")
    }
}

impl fmt::Display for ActorPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index != 0 {
                formatter.write_str("/")?;
            }
            segment.fmt(formatter)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_classes_without_rewriting_authored_names() {
        for valid in ["a", "runtime", "parser-2", "v1"] {
            assert_eq!(ActorPathSegment::new(valid).unwrap().as_str(), valid);
        }
        for invalid in [
            "",
            "Runtime",
            "two words",
            "a/b",
            "-edge",
            "edge-",
            "a--b",
            "_",
        ] {
            assert!(
                ActorPathSegment::new(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn hierarchy_has_one_exact_git_projection() {
        let path = ActorPath::parse("context-unfold/runtime/binding-snapshot").unwrap();
        assert_eq!(path.to_string(), "context-unfold/runtime/binding-snapshot");
        assert_eq!(
            path.git_branch(),
            "shoal/context-unfold/runtime/binding-snapshot"
        );
    }
}
