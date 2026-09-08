//! Bounded, object-local virtual filesystem. Paths never refer to host files.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 1024;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_DEPTH: usize = 32;

#[derive(Debug)]
pub enum FsCall {
    Exists(String),
    IsFile(String),
    IsDir(String),
    IsSymlink(String),
    Read(String),
    Stat(String),
    List(String),
    Resolve(String),
    Write { path: String, data: Vec<u8>, append: bool },
    Open { path: String, create: bool, truncate: bool },
    Mkdir { path: String, parents: bool, exist_ok: bool },
    Unlink(String),
    Rmdir(String),
    Rename { src: String, dst: String },
}

#[derive(Debug, PartialEq)]
pub enum FsReply {
    None,
    Bool(bool),
    Bytes(Vec<u8>),
    Path(String),
    Paths(Vec<String>),
    Stat { directory: bool, size: i64, modified: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FsErrorKind {
    NotFound,
    Exists,
    NotDirectory,
    IsDirectory,
    Permission,
    Invalid,
    Io,
}
#[derive(Debug)]
pub struct FsError {
    pub kind: FsErrorKind,
    pub message: String,
}
impl FsError {
    pub fn new(kind: FsErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}
impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for FsError {}
pub type FsResult = Result<FsReply, FsError>;
