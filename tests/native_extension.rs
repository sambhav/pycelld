//! A second runtime verifies the public contract without importing Monty or V8.
use celld::{
    js::{Compat, RequestBody, Worker, WorkerConfig, WorkerConfigOptions},
    native::*,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct TestRuntime;
struct TestProgram;
struct TestExecution;
static DROPPED: AtomicUsize = AtomicUsize::new(0);
impl Runtime for TestRuntime {
    fn descriptor(&self) -> &Descriptor {
        &Descriptor {
            extension: "test",
            main_module: "index.test",
            artifact_prefix: "# celld:test-v1\n",
            required_feature: "test-v1",
        }
    }
    fn compile(&self, _: &str) -> Result<Box<dyn Program>> {
        Ok(Box::new(TestProgram))
    }
    fn types(&self) -> &'static str {
        ""
    }
}
impl Program for TestProgram {
    fn fork(&self) -> Box<dyn Program> {
        Box::new(Self)
    }
    fn classes(&self) -> &[String] {
        &[]
    }
    fn start(&self, _: Invocation) -> Result<(Box<dyn Execution>, Step)> {
        Ok((
            Box::new(TestExecution),
            Step::Call(HostCall::Sleep(std::time::Duration::ZERO)),
        ))
    }
    fn error_response(&self, error: Failure, _: bool) -> Response {
        Response {
            status: error.status,
            headers: vec![],
            body: error.message.into_bytes(),
        }
    }
}
impl Execution for TestExecution {
    fn resume(&mut self, reply: HostReply) -> Result<Step> {
        match reply {
            HostReply::Value(_) => Ok(Step::Call(HostCall::Now)),
            HostReply::Timestamp(Some(now)) if now > 0 => Ok(Step::Return(Response {
                status: 200,
                headers: vec![],
                body: b"native".to_vec(),
            })),
            _ => Err("unexpected host reply".into()),
        }
    }
}
impl Drop for TestExecution {
    fn drop(&mut self) {
        DROPPED.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn registration_feature_negotiation_native_io_and_cancellation() {
    assert!(celld::protocol::validate_required_features(&["test-v1".into()]).is_err());
    register(&TestRuntime).unwrap();
    assert!(register(&TestRuntime).is_err());
    celld::protocol::validate_required_features(&["test-v1".into()]).unwrap();
    assert!(celld::protocol::validate_required_features(&["monty-native-v1".into()]).is_err());
    let config = Arc::new(WorkerConfig::new(WorkerConfigOptions {
        src: "# celld:test-v1\nsource".into(),
        script_name: "extension".into(),
        do_classes: vec![],
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
    let mut worker = Worker::load_config(config).unwrap();
    assert!(matches!(worker, Worker::Native(_)));
    for cancel in [false, true] {
        let (reply, receive) = tokio::sync::oneshot::channel();
        let (entry, mut operations) = worker.turn_begin(
            celld::WorkerJob::Fetch {
                queued_at: std::time::Instant::now(),
                url: "http://test/run".into(),
                method: "POST".into(),
                headers: vec![],
                body: RequestBody::Bytes("{}".into()),
                request_id: None,
                reply,
            },
            None,
        );
        let mut entry = entry.unwrap();
        assert!(!entry.finished());
        if cancel {
            worker.turn_cancel(&mut entry);
            drop(operations);
            entry.abandon();
            assert!(receive.await.unwrap().is_err());
        } else {
            let (id, operation) = operations.pop().unwrap();
            assert!(worker
                .turn_deliver(&mut entry, id, operation.await)
                .is_empty());
            assert_eq!(receive.await.unwrap().unwrap().body, b"native");
        }
        assert!(entry.finished());
    }
    assert_eq!(DROPPED.load(Ordering::SeqCst), 2);
}
