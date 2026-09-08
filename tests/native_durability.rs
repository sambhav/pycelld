//! Regression coverage for the native host's celld 0.4.1 durability contract.
use super::*;

struct TestProgram;
struct TestExecution(String);
impl api::Program for TestProgram {
    fn fork(&self) -> Box<dyn api::Program> {
        Box::new(Self)
    }
    fn classes(&self) -> &[String] {
        static CLASSES: OnceLock<Vec<String>> = OnceLock::new();
        CLASSES.get_or_init(|| vec!["Counter".into()])
    }
    fn start(&self, invocation: api::Invocation) -> api::Result<(Box<dyn api::Execution>, Step)> {
        let mode = invocation
            .request
            .url
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned();
        let call = match mode.as_str() {
            "file-error" => HostCall::Filesystem(api::filesystem::FsCall::Write {
                path: "note".into(), data: b"persisted".to_vec(), append: false }),
            "file-read" => HostCall::Filesystem(api::filesystem::FsCall::Read("note".into())),
            "read-error" => HostCall::Get("value".into()),
            "sync" => HostCall::Sync,
            _ => HostCall::Put("value".into(), json!(1)),
        };
        Ok((Box::new(TestExecution(mode)), Step::Call(call)))
    }
    fn error_response(&self, error: Failure, _: bool) -> api::Response {
        api::Response {
            status: 500,
            headers: vec![],
            body: error.message.into_bytes(),
        }
    }
}
impl api::Execution for TestExecution {
    fn resume(&mut self, reply: HostReply) -> api::Result<Step> {
        if let HostReply::Error(error) = reply {
            return Err(error.into());
        }
        match self.0.as_str() {
            "write-error" | "read-error" | "file-error" => Err("handler raised".into()),
            "sleep" => {
                self.0 = "done".into();
                Ok(Step::Call(HostCall::Sleep(Duration::from_secs(10))))
            }
            _ => Ok(Step::Return(api::Response {
                status: 200,
                headers: vec![],
                body: b"done".to_vec(),
            })),
        }
    }
}

fn invoke(
    worker: &mut Worker,
    scope: &str,
    method: &str,
) -> (
    InFlight,
    Vec<Op>,
    tokio::sync::oneshot::Receiver<Result<HttpResponse>>,
) {
    let (reply, receive) = tokio::sync::oneshot::channel();
    let (entry, ops) = worker.turn_begin_cell(
        CellJob::Fetch {
            scope: scope.into(),
            name: None,
            request_id: None,
            url: format!("http://test/{method}"),
            method: "POST".into(),
            body: RequestBody::Bytes(Vec::new().into()),
            headers: vec![],
            reply,
            order: None,
        },
        None,
    );
    (entry.unwrap(), ops, receive)
}

#[tokio::test]
async fn errors_cancellation_sync_and_paged_storage_preserve_durability() {
    let config = Arc::new(WorkerConfig::new(WorkerConfigOptions {
        src: String::new(),
        script_name: "native-gates".into(),
        do_classes: vec!["Counter".into()],
        bindings: vec![],
        r2_bindings: vec![],
        d1_bindings: vec![],
        kv_bindings: vec![],
        queue_bindings: vec![],
        queue_consumers: vec![],
        workflow_bindings: vec![],
        ai_binding: None,
        vars: vec![],
        node: "test".into(),
        modules: vec![],
        compat: Compat::default(),
    }));
    let mut worker = Worker {
        config,
        program: Box::new(TestProgram),
        env: json!({}),
        cells: Default::default(),
        live: Arc::new(()),
    };
    let scope = format!(
        "Counter:{}",
        durable_object_id_hex(&durable_object_id_for_name(
            &namespace_key("native-gates", "Counter"),
            "test",
        ))
    );
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cell.sqlite");
    let path = path.to_str().unwrap();
    // The native opener must honor the activation's VFS, not read a sparse
    // restored file through SQLite's default VFS.
    assert!(
        worker
            .own_cell(
                &scope,
                Some(CellStorage {
                    path,
                    epoch: 7,
                    vfs: Some("missing-test-vfs"),
                })
            )
            .is_err()
    );
    worker
        .own_cell(
            &scope,
            Some(CellStorage {
                path,
                epoch: 7,
                vfs: None,
            }),
        )
        .unwrap();
    worker.set_id_name(&scope, "test").unwrap();

    let (entry, ops, reply) = invoke(&mut worker, &scope, "write-error");
    assert!(entry.finished() && ops.is_empty());
    let response = reply.await.unwrap().unwrap();
    assert_eq!(response.status, 500);
    let committed = response
        .write_position
        .expect("a raised handler must carry its commit");

    let (entry, ops, reply) = invoke(&mut worker, &scope, "read-error");
    assert!(entry.finished() && ops.is_empty());
    let response = reply.await.unwrap().unwrap();
    assert_eq!(response.status, 500);
    assert_eq!(response.write_position, None);
    assert_eq!(response.observed_position, Some(committed));

    let (mut entry, ops, reply) = invoke(&mut worker, &scope, "sleep");
    assert!(!entry.finished());
    worker.turn_cancel(&mut entry);
    drop(ops);
    assert!(entry.finished());
    let error = reply.await.unwrap().err().expect("cancellation must fail");
    let cancelled_commit =
        failed_write_position(&error).expect("cancellation must carry committed writes");
    assert!(cancelled_commit >= committed);

    // Sync uses upstream's sync token; ordinary response positions also
    // include SQLite's data version.
    let sync_position = {
        let _cells = worker.cells.install();
        storage::sync_sample(&scope).unwrap().position
    };

    // A missing host gate must not turn sync into a local-only success.
    let (entry, ops, reply) = invoke(&mut worker, &scope, "sync");
    assert!(entry.finished() && ops.is_empty());
    let response = reply.await.unwrap().unwrap();
    assert_eq!(response.status, 500);
    assert!(
        String::from_utf8(response.body)
            .unwrap()
            .contains("no output-gate channel")
    );

    let (sender, mut tickets) = tokio::sync::mpsc::unbounded_channel();
    set_gate_tx(sender);
    for proven in [false, true] {
        let (mut entry, mut ops, reply) = invoke(&mut worker, &scope, "sync");
        assert!(!entry.finished());
        let (id, future) = ops.pop().unwrap();
        let operation = tokio::spawn(future);
        let gate = tickets.recv().await.unwrap();
        assert_eq!(gate.scope, scope);
        assert!(matches!(gate.ticket.channel, celld_logic::Channel::Sync));
        assert_eq!(gate.ticket.position, Some(sync_position));
        assert_eq!(gate.ticket.epoch, Some(7));
        assert_eq!(gate.ticket.observed, None);
        gate.reply
            .send(if proven {
                Ok(())
            } else {
                Err(celld_logic::RequestError::DurabilityUnproven)
            })
            .unwrap();
        worker.turn_deliver(&mut entry, id, operation.await.unwrap());
        let response = reply.await.unwrap().unwrap();
        assert_eq!(response.status, if proven { 200 } else { 500 });
        assert!(entry.finished());
    }
    let (entry, ops, reply) = invoke(&mut worker, &scope, "file-error");
    assert!(entry.finished() && ops.is_empty());
    let response = reply.await.unwrap().unwrap();
    assert_eq!(response.status, 500);
    let file_position = response.write_position.expect("file writes must reach the output gate even after raise");
    assert!(file_position >= committed);
    {
        let _cells = worker.cells.install();
        assert!(storage::sql_exec(&scope, "SELECT data FROM _cf_pycelld_fs", &[]).is_err());
    }
    // Release ownership and reactivate the same database at a new epoch.
    worker.own_cell(&scope, None).unwrap();
    worker.own_cell(&scope, Some(CellStorage { path, epoch: 8, vfs: None })).unwrap();
    let (entry, ops, reply) = invoke(&mut worker, &scope, "file-read");
    assert!(entry.finished() && ops.is_empty());
    let response = reply.await.unwrap().unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.write_position, None, "reading existing files must not create a new write");
    {
        let _cells = worker.cells.install();
        assert_eq!(storage::filesystem::call(&scope, api::filesystem::FsCall::Read("note".into())).unwrap(),
            api::filesystem::FsReply::Bytes(b"persisted".to_vec()));
    }
    worker.own_cell(&scope, None).unwrap();

    // A real sparse restoration through upstream's fault-in VFS. The page
    // reader stands in for bucket retrieval; no host filesystem is mounted.
    let image = std::fs::read(path).unwrap();
    let size = u16::from_be_bytes([image[16], image[17]]) as u32;
    let pages = Arc::new(FilePages { image, size: if size == 1 { 65536 } else { size },
        reads: std::sync::atomic::AtomicU64::new(0) });
    let restored = root.path().join("restored.sqlite");
    let vfs = celld_ltx::paged_vfs::next_registration_name();
    celld_ltx::paged_vfs::register_paged_vfs(&vfs, None, &restored, pages.clone()).unwrap();
    worker.own_cell(&scope, Some(CellStorage { path: restored.to_str().unwrap(), epoch: 9, vfs: Some(&vfs) })).unwrap();
    {
        let _cells = worker.cells.install();
        use api::filesystem::{FsCall, FsReply};
        assert_eq!(storage::filesystem::call(&scope, FsCall::Read("note".into())).unwrap(),
            FsReply::Bytes(b"persisted".to_vec()));
        storage::filesystem::call(&scope, FsCall::Write { path: "note".into(), data: b" restored".to_vec(), append: true }).unwrap();
    }
    assert!(pages.reads.load(Ordering::Relaxed) > 0, "restore must fault in database pages");
    worker.own_cell(&scope, None).unwrap();
    worker.own_cell(&scope, Some(CellStorage { path: restored.to_str().unwrap(), epoch: 10, vfs: Some(&vfs) })).unwrap();
    {
        let _cells = worker.cells.install();
        assert_eq!(storage::filesystem::call(&scope, api::filesystem::FsCall::Read("note".into())).unwrap(),
            api::filesystem::FsReply::Bytes(b"persisted restored".to_vec()));
    }
    worker.own_cell(&scope, None).unwrap();
    celld_ltx::paged_vfs::unregister_paged_vfs(&vfs).unwrap();
}


struct FilePages {
    image: Vec<u8>,
    size: u32,
    reads: std::sync::atomic::AtomicU64,
}
impl celld_ltx::paged_vfs::PageReader for FilePages {
    fn page_size(&self) -> u32 { self.size }
    fn commit(&self) -> u32 { (self.image.len() / self.size as usize) as u32 }
    fn read_run(&self, pgno: u32) -> celld_ltx::error::Result<Vec<(u32, Vec<u8>)>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let offset = (pgno as usize - 1) * self.size as usize;
        Ok(self.image.get(offset..offset+self.size as usize)
            .map(|bytes| vec![(pgno, bytes.to_vec())]).unwrap_or_default())
    }
}
