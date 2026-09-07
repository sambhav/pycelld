//! The Python backend loads and runs without calling Engine::init(). These
//! tests exercise the same worker facade and continuations as the live pool.
use celld::deploy::{build, Options};
use celld::js::{Compat, HttpResponse, RequestBody, Worker, WorkerConfig, WorkerConfigOptions};
use std::sync::Arc;
use tokio::sync::oneshot;

fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| celld::native::register(&celld_monty::Monty).unwrap());
}
fn config(source: &str) -> Arc<WorkerConfig> {
    register();
    Arc::new(WorkerConfig::new(WorkerConfigOptions {
        src: format!("# celld:monty-native-v1\n{source}"),
        script_name: "native-test".into(),
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
    }))
}
fn request(
    body: RequestBody,
) -> (
    celld::WorkerJob,
    oneshot::Receiver<anyhow::Result<HttpResponse>>,
) {
    let (reply, receive) = oneshot::channel();
    (
        celld::WorkerJob::Fetch {
            queued_at: std::time::Instant::now(),
            url: "http://test/run".into(),
            method: "POST".into(),
            headers: vec![],
            body,
            request_id: None,
            reply,
        },
        receive,
    )
}

#[test]
fn native_programs_run_on_multiple_threads_without_v8() {
    let config = config("from dataclasses import dataclass\n@dataclass\nclass Answer:\n    value: int\ndef run(value: int): return Answer(value + 1)");
    let workers = (0..4)
        .map(|_| Worker::load_config(config.clone()).unwrap())
        .collect::<Vec<_>>();
    let threads = workers
        .into_iter()
        .enumerate()
        .map(|(n, mut worker)| {
            assert!(matches!(worker, Worker::Native(_)));
            std::thread::spawn(move || {
                let (job, mut reply) =
                    request(RequestBody::Bytes(format!("{{\"value\":{n}}}").into()));
                let (entry, operations) = worker.turn_begin(job, None);
                assert!(entry.unwrap().finished());
                assert!(operations.is_empty());
                assert_eq!(
                    reply.try_recv().unwrap().unwrap().body,
                    format!("{{\"value\":{}}}", n + 1).as_bytes()
                );
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[tokio::test]
async fn native_suspension_body_limits_and_cancellation_release_capacity() {
    let mut worker = Worker::load_config(config(
        "async def run(ctx):\n    await ctx.sleep(0)\n    return b'complete'",
    ))
    .unwrap();
    let (job, reply) = request(RequestBody::Bytes("{}".into()));
    let (entry, mut ops) = worker.turn_begin(job, None);
    let mut entry = entry.unwrap();
    assert!(!entry.finished());
    assert_eq!(ops.len(), 1);
    let (id, op) = ops.pop().unwrap();
    assert!(worker.turn_deliver(&mut entry, id, op.await).is_empty());
    assert_eq!(reply.await.unwrap().unwrap().body, b"complete");
    assert!(entry.finished());

    let mut pending = Vec::new();
    for _ in 0..256 {
        let (job, reply) = request(RequestBody::Bytes("{}".into()));
        let (entry, ops) = worker.turn_begin(job, None);
        assert!(!entry.as_ref().unwrap().finished());
        pending.push((entry.unwrap(), ops, reply));
    }
    let (job, reply) = request(RequestBody::Bytes("{}".into()));
    assert!(worker.turn_begin(job, None).0.is_none());
    assert_eq!(reply.await.unwrap().unwrap().status, 503);
    for (mut entry, ops, reply) in pending {
        worker.turn_cancel(&mut entry);
        drop(ops);
        entry.abandon();
        assert!(reply.await.unwrap().is_err());
        assert!(entry.finished());
    }
    let stream = Box::pin(futures_util::stream::iter([
        Ok(vec![b'x'; 1024 * 1024]),
        Ok(vec![b'x']),
    ]));
    let id = celld::js::register_body_stream(stream);
    let (job, reply) = request(RequestBody::Stream(id));
    let (entry, mut ops) = worker.turn_begin(job, None);
    let mut entry = entry.unwrap();
    let (id, op) = ops.pop().unwrap();
    worker.turn_deliver(&mut entry, id, op.await);
    assert_eq!(reply.await.unwrap().unwrap().status, 413);
    assert!(entry.finished());
}

fn options(config: std::path::PathBuf) -> Options {
    Options {
        config: Some(config),
        bucket: None,
        endpoint: None,
        region: None,
        dry_run: true,
        json: false,
    }
}
#[test]
fn monty_builds_native_python_without_javascript_modules() {
    register();
    let root = tempfile::tempdir().unwrap();
    let entry = root.path().join("worker.py");
    let source = include_str!("../examples/monty/worker.py");
    std::fs::write(&entry, source).unwrap();
    let config = root.path().join("wrangler.json");
    std::fs::write(&config, r#"{"name":"monty","main":"worker.py"}"#).unwrap();
    let built = build(&options(config.clone())).unwrap();
    assert_eq!(built.manifest.do_classes, vec!["Counter"]);
    assert_eq!(built.manifest.sqlite_classes, vec!["Counter"]);
    assert!(built
        .manifest
        .required_features
        .iter()
        .any(|f| f == "monty-native-v1"));
    assert!(!built
        .modules
        .iter()
        .any(|(name, _)| name.ends_with(".wasm")));
    assert_eq!(built.manifest.main_module.as_deref(), Some("index.py"));
    assert_eq!(built.modules.len(), 1);
    assert!(built.modules[0].1.starts_with(b"# celld:monty-native-v1\n"));
    std::fs::write(&entry, source.replace("Hello,", "Welcome,")).unwrap();
    let updated = build(&options(config.clone())).unwrap();
    assert_ne!(built.version, updated.version);
    assert_eq!(updated.modules.len(), 1);
    for setting in [
        r#""python_runtime":"monty""#,
        r#""no_bundle":true"#,
        r#""durable_objects":{"bindings":[{"name":"COUNTER","class_name":"Counter"}]}"#,
    ] {
        std::fs::write(
            &config,
            format!(r#"{{"name":"monty","main":"worker.py",{setting}}}"#),
        )
        .unwrap();
        assert!(build(&options(config.clone())).is_err(), "{setting}");
    }
    std::fs::write(&config, r#"{"name":"monty","main":"worker.py"}"#).unwrap();
    std::fs::write(&entry, "invalid Python!").unwrap();
    assert!(build(&options(config)).is_err());
}
