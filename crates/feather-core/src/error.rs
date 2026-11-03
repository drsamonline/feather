use std::fmt;

/// Everything that can go wrong between "open this file" and "here are pixels".
///
/// Deliberately small: a viewer has no business inventing a taxonomy of
/// document failures. If it is not one of these five, it is a bug.
#[derive(Debug)]
pub enum DocumentError {
    /// The router found no pipeline for this file.
    Unsupported(String),
    /// The file could not be read from disk.
    Io(String),
    /// The engine refused to parse the bytes.
    Decode(String),
    /// The document is encrypted and no password was supplied.
    Password,
    /// Asked for a page that does not exist.
    OutOfRange { page: usize, count: usize },
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocumentError::Unsupported(what) => write!(f, "unsupported format: {what}"),
            DocumentError::Io(msg) => write!(f, "io error: {msg}"),
            DocumentError::Decode(msg) => write!(f, "could not decode document: {msg}"),
            DocumentError::Password => write!(f, "document is password protected"),
            DocumentError::OutOfRange { page, count } => {
                write!(f, "page {page} is out of range (document has {count} pages)")
            }
        }
    }
}

impl std::error::Error for DocumentError {}

impl From<std::io::Error> for DocumentError {
    fn from(e: std::io::Error) -> Self {
        DocumentError::Io(e.to_string())
    }
}
