use std::fmt;

/// An issue key such as "PROJ-123".
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IssueKey(String);

impl From<&str> for IssueKey {
    fn from(key: &str) -> Self {
        Self(key.to_owned())
    }
}

impl fmt::Display for IssueKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
