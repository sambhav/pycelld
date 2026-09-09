//! Payload accounting at the native boundary. Counts UTF-8 strings, serialized
//! JSON, byte buffers and HTTP headers without allocating a second JSON buffer.
use crate::{
    HostCall, HostReply, Request, Response,
    filesystem::{FsCall, FsReply},
};
fn sum(values: impl IntoIterator<Item = usize>) -> usize {
    values.into_iter().fold(0, usize::saturating_add)
}
fn headers(values: &[(String, String)]) -> usize {
    sum(values.iter().map(|(k, v)| k.len().saturating_add(v.len())))
}
fn json(value: &serde_json::Value) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    if serde_json::to_writer(&mut counter, value).is_err() {
        return usize::MAX;
    }
    counter.0
}
impl Request {
    pub fn payload_bytes(&self) -> usize {
        sum([
            self.url.len(),
            self.method.len(),
            headers(&self.headers),
            self.body.len(),
        ])
    }
}
impl Response {
    pub fn payload_bytes(&self) -> usize {
        headers(&self.headers).saturating_add(self.body.len())
    }
}
impl HostCall {
    pub fn payload_bytes(&self) -> usize {
        use HostCall::*;
        match self {
            Get(s) | Delete(s) | Log(s) => s.len(),
            Put(k, v) => k.len().saturating_add(json(v)),
            List { prefix, .. } => prefix.len(),
            Sql { query, bindings } => query.len().saturating_add(sum(bindings.iter().map(json))),
            Fetch(r) => r.payload_bytes(),
            CallObject {
                object,
                method,
                body,
            } => sum([
                object.class.len(),
                object.id.len(),
                method.len(),
                body.len(),
            ]),
            Filesystem(call) => match call {
                FsCall::Exists(p)
                | FsCall::IsFile(p)
                | FsCall::IsDir(p)
                | FsCall::IsSymlink(p)
                | FsCall::Read(p)
                | FsCall::Stat(p)
                | FsCall::List(p)
                | FsCall::Resolve(p)
                | FsCall::Unlink(p)
                | FsCall::Rmdir(p) => p.len(),
                FsCall::Open { path, .. } | FsCall::Mkdir { path, .. } => path.len(),
                FsCall::Write { path, data, .. } => path.len().saturating_add(data.len()),
                FsCall::Rename { src, dst } => src.len().saturating_add(dst.len()),
            },
            _ => 0,
        }
    }
}
impl HostReply {
    pub fn payload_bytes(&self) -> usize {
        match self {
            Self::Fetch(r) | Self::Object(r) => r.payload_bytes(),
            Self::Value(v) => json(v),
            Self::Error(s) => s.len(),
            Self::Timestamp(_) => 0,
            Self::Filesystem(Err(e)) => e.message.len(),
            Self::Filesystem(Ok(reply)) => match reply {
                FsReply::Bytes(b) => b.len(),
                FsReply::Path(p) => p.len(),
                FsReply::Paths(paths) => sum(paths.iter().map(String::len)),
                _ => 0,
            },
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accounts_headers_urls_and_encoded_values() {
        assert_eq!(
            Request {
                url: "url".into(),
                method: "GET".into(),
                headers: vec![("a".into(), "bc".into())],
                body: vec![0; 4]
            }
            .payload_bytes(),
            13
        );
        assert_eq!(
            HostCall::Put("key".into(), serde_json::json!("a\nb")).payload_bytes(),
            9
        );
        assert_eq!(
            HostReply::Value(serde_json::json!([1, 2])).payload_bytes(),
            5
        );
    }
}
