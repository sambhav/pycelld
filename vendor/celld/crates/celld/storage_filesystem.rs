//! Object-local files are ordinary replicated SQLite rows, never host paths.
use super::{with_application_storage_write, with_batch_savepoint};
use celld_runtime::filesystem::*;
use rusqlite::{Connection, OptionalExtension, params};

fn fail(kind: FsErrorKind, path: &str) -> anyhow::Error {
    FsError::new(kind, path).into()
}

fn normalize(path: &str) -> anyhow::Result<String> {
    if path.len() > MAX_PATH_BYTES || path.contains('\0') {
        return Err(fail(FsErrorKind::Invalid, "invalid or oversized path"));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => { if parts.pop().is_none() { return Err(fail(FsErrorKind::Permission, "path escapes the object root")); } }
            part => parts.push(part),
        }
    }
    if parts.len() > MAX_DEPTH {
        return Err(fail(FsErrorKind::Invalid, "path depth limit exceeded"));
    }
    let normalized = format!("/{}", parts.join("/"));
    if normalized.len() > MAX_PATH_BYTES {
        return Err(fail(FsErrorKind::Invalid, "normalized path exceeds 4096 bytes"));
    }
    Ok(normalized)
}
fn parent(path: &str) -> &str {
    match path.rsplit_once('/').unwrap().0 { "" => "/", value => value }
}

struct Entry { directory: bool, size: i64, modified: f64 }
fn entry(c: &Connection, path: &str) -> anyhow::Result<Option<Entry>> {
    Ok(c.query_row("SELECT directory,length(data),modified FROM _cf_pycelld_fs WHERE path=?1", [path], |r| {
        Ok(Entry { directory: r.get(0)?, size: r.get(1)?, modified: r.get(2)? })
    }).optional()?)
}
fn required(c: &Connection, path: &str) -> anyhow::Result<Entry> {
    // Check ancestors so /file/child produces NotADirectoryError.
    if path != "/" {
        let mut prefix = String::new();
        let parts: Vec<_> = path[1..].split('/').collect();
        for part in &parts[..parts.len()-1] {
            prefix.push('/'); prefix.push_str(part);
            match entry(c, &prefix)? {
                Some(e) if !e.directory => return Err(fail(FsErrorKind::NotDirectory, &prefix)),
                None => return Err(fail(FsErrorKind::NotFound, &prefix)),
                _ => {}
            }
        }
    }
    entry(c, path)?.ok_or_else(|| fail(FsErrorKind::NotFound, path))
}
fn directory(c: &Connection, path: &str) -> anyhow::Result<()> {
    if !required(c, path)?.directory { return Err(fail(FsErrorKind::NotDirectory, path)); }
    Ok(())
}
fn file(c: &Connection, path: &str) -> anyhow::Result<Entry> {
    let e = required(c, path)?;
    if e.directory { return Err(fail(FsErrorKind::IsDirectory, path)); }
    Ok(e)
}
fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}
fn touch_parent(c: &Connection, path: &str) -> anyhow::Result<()> {
    c.execute("UPDATE _cf_pycelld_fs SET modified=?1 WHERE path=?2", params![now(), parent(path)])?;
    Ok(())
}
fn insert(c: &Connection, path: &str, directory: bool) -> anyhow::Result<()> {
    let count: i64 = c.query_row("SELECT count(*) FROM _cf_pycelld_fs", [], |r| r.get(0))?;
    if count >= MAX_ENTRIES as i64 { return Err(fail(FsErrorKind::Io, "filesystem entry quota exceeded")); }
    c.execute("INSERT INTO _cf_pycelld_fs(path,directory,data,modified) VALUES(?1,?2,x'',?3)", params![path,directory,now()])?;
    touch_parent(c, path)
}
fn write(c: &Connection, path: &str, data: &[u8], append: bool) -> anyhow::Result<()> {
    directory(c, parent(path))?;
    let old = match entry(c, path)? {
        Some(e) if e.directory => return Err(fail(FsErrorKind::IsDirectory, path)),
        Some(e) => e.size as usize,
        None => { insert(c, path, false)?; 0 }
    };
    let size = if append { old.saturating_add(data.len()) } else { data.len() };
    if size > MAX_FILE_BYTES { return Err(fail(FsErrorKind::Io, "file exceeds 1 MiB")); }
    let total: i64 = c.query_row("SELECT coalesce(sum(length(data)),0) FROM _cf_pycelld_fs", [], |r| r.get(0))?;
    if total as usize - old + size > MAX_TOTAL_BYTES { return Err(fail(FsErrorKind::Io, "filesystem byte quota exceeded")); }
    let query = if append {
        "UPDATE _cf_pycelld_fs SET data=CAST(data || ?1 AS BLOB),modified=?2 WHERE path=?3"
    } else {
        "UPDATE _cf_pycelld_fs SET data=?1,modified=?2 WHERE path=?3"
    };
    c.execute(query, params![data,now(),path])?;
    Ok(())
}
fn descendants(c: &Connection, path: &str) -> anyhow::Result<Vec<String>> {
    let prefix = if path == "/" { "/".to_owned() } else { format!("{path}/") };
    let mut stmt = c.prepare("SELECT path FROM _cf_pycelld_fs WHERE substr(path,1,length(?1))=?1 AND path!=?2 ORDER BY path")?;
    let paths = stmt.query_map(params![prefix,path], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(paths)
}

pub(crate) fn call(scope: &str, call: FsCall) -> FsResult {
    with_application_storage_write(scope, |c| with_batch_savepoint(c, |c| execute(c, call)))
        .map_err(|e| match e.downcast::<FsError>() {
            Ok(error) => error,
            Err(error) => FsError::new(FsErrorKind::Io, error.to_string()),
        })
}
fn execute(c: &Connection, call: FsCall) -> anyhow::Result<FsReply> {
    // This table is protected by celld's reserved _cf_ SQL authorizer. Schema
    // creation and every operation join the current transaction/savepoint.
    c.execute_batch("CREATE TABLE IF NOT EXISTS _cf_pycelld_fs (
        path TEXT PRIMARY KEY, directory INTEGER NOT NULL, data BLOB NOT NULL,
        modified REAL NOT NULL
    ) WITHOUT ROWID;")?;
    c.execute("INSERT OR IGNORE INTO _cf_pycelld_fs VALUES('/',1,x'',?1)", [now()])?;
    use FsCall::*;
    if let Exists(p) | IsFile(p) | IsDir(p) | IsSymlink(p) = &call {
        let p = normalize(p)?;
        let e = match required(c, &p) {
            Ok(e) => Some(e),
            Err(error) if error.downcast_ref::<FsError>().is_some_and(|e|
                matches!(e.kind, FsErrorKind::NotFound | FsErrorKind::NotDirectory)) => None,
            Err(error) => return Err(error),
        };
        return Ok(FsReply::Bool(match call {
            Exists(_) => e.is_some(),
            IsFile(_) => e.is_some_and(|e| !e.directory),
            IsDir(_) => e.is_some_and(|e| e.directory),
            _ => false,
        }));
    }
    match call {
        Exists(_) | IsFile(_) | IsDir(_) | IsSymlink(_) => unreachable!(),
        Read(path) => {
            let path = normalize(&path)?; file(c, &path)?;
            Ok(FsReply::Bytes(c.query_row("SELECT data FROM _cf_pycelld_fs WHERE path=?1", [path], |r| r.get(0))?))
        }
        Stat(path) => {
            let path = normalize(&path)?; let e = required(c, &path)?;
            Ok(FsReply::Stat { directory: e.directory, size: e.size, modified: e.modified })
        }
        List(path) => {
            let path = normalize(&path)?; directory(c, &path)?;
            let paths: Vec<_> = descendants(c, &path)?.into_iter().filter(|p| parent(p) == path).collect();
            if paths.iter().map(|p| p.len() + 64).sum::<usize>() > MAX_FILE_BYTES {
                return Err(fail(FsErrorKind::Io, "directory listing exceeds 1 MiB"));
            }
            Ok(FsReply::Paths(paths))
        }
        Resolve(path) => Ok(FsReply::Path(normalize(&path)?)),
        Write { path, data, append } => {
            let path = normalize(&path)?; write(c, &path, &data, append)?; Ok(FsReply::None)
        }
        Open { path, create, truncate } => {
            let path = normalize(&path)?;
            if truncate || (create && entry(c, &path)?.is_none()) { write(c, &path, &[], false)?; }
            file(c, &path)?; Ok(FsReply::Path(path))
        }
        Mkdir { path, parents, exist_ok } => {
            let path = normalize(&path)?;
            if let Some(e) = entry(c, &path)? {
                if exist_ok && e.directory { return Ok(FsReply::None); }
                return Err(fail(FsErrorKind::Exists, &path));
            }
            if parents {
                let mut prefix = String::new();
                for part in path[1..].split('/') {
                    prefix.push('/'); prefix.push_str(part);
                    if entry(c, &prefix)?.is_none() { insert(c, &prefix, true)?; }
                    directory(c, &prefix)?;
                }
            } else { directory(c, parent(&path))?; insert(c, &path, true)?; }
            Ok(FsReply::None)
        }
        Unlink(path) => {
            let path = normalize(&path)?; file(c, &path)?;
            c.execute("DELETE FROM _cf_pycelld_fs WHERE path=?1", [&path])?;
            touch_parent(c, &path)?; Ok(FsReply::None)
        }
        Rmdir(path) => {
            let path = normalize(&path)?; directory(c, &path)?;
            if path == "/" { return Err(fail(FsErrorKind::Permission, "cannot remove the root")); }
            if !descendants(c, &path)?.is_empty() { return Err(fail(FsErrorKind::Io, "directory is not empty")); }
            c.execute("DELETE FROM _cf_pycelld_fs WHERE path=?1", [&path])?;
            touch_parent(c, &path)?; Ok(FsReply::None)
        }
        Rename { src, dst } => {
            let src = normalize(&src)?; let dst = normalize(&dst)?;
            let source = required(c, &src)?;
            if src == dst { return Ok(FsReply::Path(dst)); }
            if src == "/" || dst == "/" { return Err(fail(FsErrorKind::Permission, "cannot rename the root")); }
            if dst.starts_with(&format!("{src}/")) { return Err(fail(FsErrorKind::Invalid, "cannot move a directory inside itself")); }
            directory(c, parent(&dst))?;
            if let Some(target) = entry(c, &dst)? {
                if target.directory != source.directory {
                    return Err(fail(if target.directory { FsErrorKind::IsDirectory } else { FsErrorKind::NotDirectory }, &dst));
                }
                if target.directory && !descendants(c, &dst)?.is_empty() { return Err(fail(FsErrorKind::Io, "destination directory is not empty")); }
                c.execute("DELETE FROM _cf_pycelld_fs WHERE path=?1", [&dst])?;
            }
            // Paths are bounded by 1024 entries. Validate the entire move before
            // changing any row; the savepoint also restores an overwritten target.
            let children = descendants(c, &src)?;
            for old in &children { normalize(&format!("{dst}{}", &old[src.len()..]))?; }
            for old in children.into_iter().chain(std::iter::once(src.clone())) {
                let new = format!("{dst}{}", &old[src.len()..]);
                c.execute("UPDATE _cf_pycelld_fs SET path=?1 WHERE path=?2", params![new,old])?;
            }
            touch_parent(c, &src)?; touch_parent(c, &dst)?;
            Ok(FsReply::Path(dst))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(c: &Connection, call: FsCall) -> anyhow::Result<FsReply> {
        with_batch_savepoint(c, |c| execute(c, call))
    }
    fn put(path: &str, data: &[u8]) -> FsCall {
        FsCall::Write { path: path.into(), data: data.into(), append: false }
    }
    fn mkdir(path: &str) -> FsCall {
        FsCall::Mkdir { path: path.into(), parents: true, exist_ok: true }
    }
    fn kind(error: anyhow::Error) -> FsErrorKind { error.downcast::<FsError>().unwrap().kind }
    #[test]
    fn hierarchy_binary_rename_and_errors() {
        let c = Connection::open_in_memory().unwrap();
        run(&c, mkdir("a/b")).unwrap();
        run(&c, put("a/b/file", &[0,255])).unwrap();
        run(&c, FsCall::Write { path: "a/b/file".into(), data: vec![128], append: true }).unwrap();
        assert_eq!(run(&c, FsCall::Read("a/b/file".into())).unwrap(), FsReply::Bytes(vec![0,255,128]));
        run(&c, FsCall::Rename { src: "a".into(), dst: "moved".into() }).unwrap();
        assert_eq!(run(&c, FsCall::List("moved/b".into())).unwrap(), FsReply::Paths(vec!["/moved/b/file".into()]));
        assert_eq!(kind(run(&c, FsCall::Read("moved".into())).unwrap_err()), FsErrorKind::IsDirectory);
        assert_eq!(kind(run(&c, FsCall::Read("moved/b/file/x".into())).unwrap_err()), FsErrorKind::NotDirectory);
        assert_eq!(kind(run(&c, put("../escape", &[])).unwrap_err()), FsErrorKind::Permission);
        assert_eq!(kind(run(&c, FsCall::Rmdir("moved".into())).unwrap_err()), FsErrorKind::Io);
        assert!(run(&c, FsCall::Rename { src: "moved".into(), dst: "moved/b/x".into() }).is_err());
        run(&c, FsCall::Unlink("moved/b/file".into())).unwrap();
        run(&c, FsCall::Rmdir("moved/b".into())).unwrap();
    }
    #[test]
    fn quotas_and_rename_failures_are_atomic() {
        let c = Connection::open_in_memory().unwrap();
        let bytes = vec![255; MAX_FILE_BYTES];
        for i in 0..16 { run(&c, put(&format!("f{i}"), &bytes)).unwrap(); }
        assert!(run(&c, put("overflow", &[1])).is_err());
        assert_eq!(run(&c, FsCall::Exists("overflow".into())).unwrap(), FsReply::Bool(false));
        assert!(run(&c, FsCall::Write { path: "f0".into(), data: vec![1], append: true }).is_err());
        assert_eq!(run(&c, FsCall::Read("f0".into())).unwrap(), FsReply::Bytes(bytes));
        run(&c, FsCall::Unlink("f0".into())).unwrap();
        run(&c, put("overflow", &[1])).unwrap();
        let deep = std::iter::repeat_n("d", MAX_DEPTH-1).collect::<Vec<_>>().join("/");
        run(&c, mkdir(&deep)).unwrap();
        run(&c, mkdir("src/child")).unwrap();
        let dst = format!("{deep}/target");
        run(&c, mkdir(&dst)).unwrap();
        assert!(run(&c, FsCall::Rename { src: "src".into(), dst: dst.clone() }).is_err());
        assert_eq!(run(&c, FsCall::IsDir(dst)).unwrap(), FsReply::Bool(true));
        assert_eq!(run(&c, FsCall::IsDir("src/child".into())).unwrap(), FsReply::Bool(true));
    }
    #[test]
    fn entry_and_path_limits_are_enforced_without_partial_changes() {
        let c = Connection::open_in_memory().unwrap();
        for i in 0..MAX_ENTRIES-1 { run(&c, put(&format!("f{i}"), &[])).unwrap(); }
        assert!(run(&c, mkdir("extra/child")).is_err());
        assert_eq!(run(&c, FsCall::Exists("extra".into())).unwrap(), FsReply::Bool(false));
        run(&c, FsCall::Unlink("f0".into())).unwrap();
        run(&c, put("replacement", &[])).unwrap();
        assert!(normalize(&"x".repeat(MAX_PATH_BYTES)).is_err());
        assert!(normalize("bad\0name").is_err());
        assert!(normalize(&std::iter::repeat_n("d", MAX_DEPTH+1).collect::<Vec<_>>().join("/")).is_err());
        assert_eq!(normalize("notes/../file").unwrap(), "/file");
    }
    #[test]
    fn files_and_other_storage_share_rollback_and_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cell.sqlite");
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE kv(value TEXT); INSERT INTO kv VALUES('old');").unwrap();
        run(&c, put("note", b"old")).unwrap();
        super::super::without_sql_authorizer(&c, || c.execute_batch("BEGIN IMMEDIATE; UPDATE kv SET value='new';")).unwrap();
        run(&c, put("note", b"new")).unwrap();
        run(&c, mkdir("temporary/folder")).unwrap();
        super::super::without_sql_authorizer(&c, || c.execute_batch("ROLLBACK")).unwrap();
        assert_eq!(c.query_row("SELECT value FROM kv", [], |r| r.get::<_,String>(0)).unwrap(), "old");
        drop(c);
        let c = Connection::open(&path).unwrap();
        assert_eq!(run(&c, FsCall::Read("note".into())).unwrap(), FsReply::Bytes(b"old".to_vec()));
        assert_eq!(run(&c, FsCall::Exists("temporary".into())).unwrap(), FsReply::Bool(false));
        let other = Connection::open_in_memory().unwrap();
        assert_eq!(run(&other, FsCall::Exists("note".into())).unwrap(), FsReply::Bool(false));
    }
}
