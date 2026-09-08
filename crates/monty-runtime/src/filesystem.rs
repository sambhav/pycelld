//! Translate Monty's public OS calls without exposing interpreter types to celld.
use celld_runtime::filesystem::{FsCall, FsError, FsErrorKind, FsReply, FsResult};
use monty_types::{ExcType, ExtFunctionResult, FileMode, MontyException, MontyFileHandle, MontyObject, OsFunctionCall};

pub(crate) enum ResultType {
    Value,
    Text,
    Written(usize),
    Open(FileMode),
}

pub(crate) fn request(call: OsFunctionCall) -> Result<(FsCall, ResultType), MontyException> {
    use OsFunctionCall::*;
    let mut result = ResultType::Value;
    let request = match call {
        Exists(p) => FsCall::Exists(p.into_string()),
        IsFile(p) => FsCall::IsFile(p.into_string()),
        IsDir(p) => FsCall::IsDir(p.into_string()),
        IsSymlink(p) => FsCall::IsSymlink(p.into_string()),
        ReadText(p) => { result = ResultType::Text; FsCall::Read(p.into_string()) }
        ReadBytes(p) => FsCall::Read(p.into_string()),
        Stat(p) => FsCall::Stat(p.into_string()),
        Iterdir(p) => FsCall::List(p.into_string()),
        Resolve(p) | Absolute(p) => FsCall::Resolve(p.into_string()),
        WriteText(a) | AppendText(a) => {
            // The append flag is supplied by the caller before consuming the OS call.
            result = ResultType::Written(a.data.chars().count());
            FsCall::Write { path: a.path.into_string(), data: a.data.into_bytes(), append: false }
        }
        WriteBytes(a) | AppendBytes(a) => {
            result = ResultType::Written(a.data.len());
            FsCall::Write { path: a.path.into_string(), data: a.data, append: false }
        }
        Open(a) => {
            if matches!(a.mode, FileMode::ReadUpdate(_) | FileMode::WriteUpdate(_) | FileMode::AppendUpdate(_)) {
                return Err(MontyException::new(ExcType::ValueError, Some("update file modes are not supported".into())));
            }
            result = ResultType::Open(a.mode);
            FsCall::Open { path: a.path.into_string(), create: a.mode.create(), truncate: a.mode.truncate() }
        }
        Mkdir(a) => FsCall::Mkdir { path: a.path.into_string(), parents: a.parents, exist_ok: a.exist_ok },
        Unlink(p) => FsCall::Unlink(p.into_string()),
        Rmdir(p) => FsCall::Rmdir(p.into_string()),
        Rename(a) => FsCall::Rename { src: a.src.into_string(), dst: a.dst.into_string() },
        other => return Err(other.on_no_handler()),
    };
    Ok((request, result))
}

pub(crate) fn response(reply: FsResult, result: ResultType) -> ExtFunctionResult {
    let value = reply.and_then(|reply| {
        Ok(match (reply, result) {
            (FsReply::Bytes(bytes), ResultType::Text) => MontyObject::String(String::from_utf8(bytes)
                .map_err(|_| FsError::new(FsErrorKind::Invalid, "file is not valid UTF-8"))?),
            (FsReply::None, ResultType::Written(count)) => MontyObject::Int(count as i64),
            (FsReply::Path(path), ResultType::Open(mode)) => MontyObject::FileHandle(MontyFileHandle { path, mode, position: 0 }),
            (FsReply::None, _) => MontyObject::None,
            (FsReply::Bool(value), _) => MontyObject::Bool(value),
            (FsReply::Bytes(value), _) => MontyObject::Bytes(value),
            (FsReply::Path(value), _) => MontyObject::Path(value),
            (FsReply::Paths(value), _) => MontyObject::List(value.into_iter().map(MontyObject::Path).collect()),
            (FsReply::Stat { directory, size, modified }, _) => if directory {
                monty_types::dir_stat(0o755, modified)
            } else {
                monty_types::file_stat(0o644, size, modified)
            },
        })
    });
    match value {
        Ok(value) => ExtFunctionResult::Return(value),
        Err(error) => ExtFunctionResult::Error(MontyException::new(match error.kind {
            FsErrorKind::NotFound => ExcType::FileNotFoundError,
            FsErrorKind::Exists => ExcType::FileExistsError,
            FsErrorKind::NotDirectory => ExcType::NotADirectoryError,
            FsErrorKind::IsDirectory => ExcType::IsADirectoryError,
            FsErrorKind::Permission => ExcType::PermissionError,
            FsErrorKind::Invalid => ExcType::ValueError,
            FsErrorKind::Io => ExcType::OSError,
        }, Some(error.message))),
    }
}
