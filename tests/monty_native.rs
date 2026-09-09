//! The Python backend loads and runs without calling Engine::init(). These
//! tests exercise the same worker facade and continuations as the live pool.
use celld::deploy::{build, Options};
use celld::js::{Compat, HttpResponse, RequestBody, Worker, WorkerConfig, WorkerConfigOptions};
use celld::pool::Pool;
use celld_logic::isolate::PoolLimits;
use std::sync::Arc;
use tokio::sync::oneshot;

fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        celld::native::register(celld_monty::Monty::new().with_fetch_middleware(|_, request| {
            if request.url.host_str() == Some("127.0.0.1") {
                celld_monty::FetchDecision::Forward(request)
            } else {
                celld_monty::FetchDecision::Deny("test host allows only loopback".into())
            }
        })).unwrap();
    });
}
// Production's process domain retains its first Tokio handle, including the
// HTTP stream sweeper. Independent short-lived test runtimes can close that
// shared service under another test. Keep one runtime alive, as the daemon does.
fn run_async(test: impl std::future::Future<Output = ()>) {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        celld::asyncrt::set_host_handle(runtime.handle().clone());
        runtime
    });
    runtime.block_on(test);
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

#[test]
fn native_suspension_body_limits_and_cancellation_release_capacity() {
    run_async(async {
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
        let id = celld::js::register_body_stream(stream).unwrap();
        let (job, reply) = request(RequestBody::Stream(id));
        let (entry, mut ops) = worker.turn_begin(job, None);
        let mut entry = entry.unwrap();
        let (id, op) = ops.pop().unwrap();
        worker.turn_deliver(&mut entry, id, op.await);
        assert_eq!(reply.await.unwrap().unwrap().status, 413);
        assert!(entry.finished());
    });
}

fn pool(config: Arc<WorkerConfig>, density: usize) -> Pool {
    Pool::new(
        PoolLimits {
            grow_at: 2,
            shrink_under: 1,
            max_stateless: 1,
            max_requests: None,
            max_cells: density,
        },
        std::time::Duration::ZERO,
        Box::new(move || {
            let worker = Worker::load_config(config.clone())?;
            assert!(matches!(worker, Worker::Native(_)));
            Ok(worker)
        }),
    )
}

#[test]
fn native_cell_packing_keeps_the_density_bound_under_concurrent_activation() {
    let config = config("def run(): return 1");
    for density in [1, 2, 32] {
        let pool = Arc::new(pool(config.clone(), density));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let pool = pool.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..5)
                        .map(|_| pool.place_cell().unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let residents = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        let mut occupancy = std::collections::BTreeMap::new();
        for resident in &residents {
            *occupancy.entry(resident.slot().heap_id()).or_insert(0) += 1;
        }
        assert_eq!(residents.len(), 40);
        assert_eq!(occupancy.len(), 40_usize.div_ceil(density));
        assert!(occupancy.values().all(|&cells| cells <= density));
        // The stateless ceiling of one must not constrain cell placement.
        assert_eq!(pool.live(), occupancy.len());
        drop(residents);
        pool.reap_empty();
        assert!(pool.is_drained());
    }
}

#[test]
fn native_packing_drains_suspended_workers_and_preserves_global_identity() {
    run_async(async {
        let config = config("async def run(ctx):\n    await ctx.sleep(0)\n    return b'complete'");
        let pool = pool(config.clone(), 3);
        let a = pool.place_cell().unwrap();
        let b = pool.place_cell().unwrap();
        let c = pool.place_cell().unwrap();
        let d = pool.place_cell().unwrap();
        assert_eq!(a.slot().heap_id(), c.slot().heap_id());
        assert_ne!(a.slot().heap_id(), d.slot().heap_id());

        // Eviction has opened holes in two workers. Fill the fullest one so the
        // sparse worker can reach zero instead of continually being refilled.
        drop(b);
        let e = pool.place_cell().unwrap();
        assert_eq!(e.slot().heap_id(), a.slot().heap_id());

        // A distinct script/generation may reuse a slot index, never its identity.
        let other = self::pool(config, 3);
        let other_cell = other.place_cell().unwrap();
        assert_eq!(other_cell.slot().id, a.slot().id);
        assert_ne!(other_cell.slot().heap_id(), a.slot().heap_id());

        let sparse = d.slot().clone();
        let affiliation = sparse.affiliate();
        let (job, reply) = request(RequestBody::Bytes("{}".into()));
        let (entry, mut ops) = sparse.turn(|worker| worker.turn_begin(job, None)).await;
        let mut entry = entry.unwrap();
        assert!(!entry.finished());
        assert_eq!(ops.len(), 1);
        drop(d);
        pool.reap_empty();
        assert!(sparse.is_retiring());
        assert!(!a.slot().is_retiring());

        // An empty but suspended worker cannot be freed or reused.
        let f = pool.place_cell().unwrap();
        assert_ne!(f.slot().id, sparse.id);
        let (id, operation) = ops.pop().unwrap();
        let result = operation.await;
        sparse
            .turn(|worker| worker.turn_deliver(&mut entry, id, result))
            .await;
        assert!(entry.finished());
        assert_eq!(reply.await.unwrap().unwrap().body, b"complete");
        drop(entry);
        drop(affiliation);
        drop(f);
        pool.reap_empty();

        // Once drained, the stable slot index is reusable with a fresh identity.
        let replacement = pool.place_cell().unwrap();
        assert_eq!(replacement.slot().id, sparse.id);
        assert_ne!(replacement.slot().heap_id(), sparse.heap_id());
        drop((a, c, e, replacement, other_cell));
        pool.reap_empty();
        other.reap_empty();
        assert!(pool.is_drained());
        assert!(other.is_drained());
    });
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
        .any(|f| f == "monty-http-v1"));
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

#[test]
fn package_deployments_discover_submodule_classes_and_version_every_source() {
    register();
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("shop");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(package.join("__init__.py"), "from .handlers import run\n__all__ = ['run']").unwrap();
    std::fs::write(package.join("handlers.py"), "from .objects import Counter\ndef run(ctx, id: str): return Counter(id, ctx).read()").unwrap();
    std::fs::write(package.join("objects.py"), "from celld import Context\nclass Counter:\n    def __init__(self, id: str, ctx: Context):\n        self.id = id\n        self._ctx = ctx\n    def read(self) -> int: return 42").unwrap();
    let config = root.path().join("wrangler.json");
    std::fs::write(&config, r#"{"name":"shop","main":"shop"}"#).unwrap();
    let built = build(&options(config.clone())).unwrap();
    assert_eq!(built.manifest.do_classes, ["shop.objects.Counter"]);
    assert_eq!(built.manifest.sqlite_classes, ["shop.objects.Counter"]);
    assert_eq!(built.modules.len(), 1);
    assert!(built.manifest.required_features.iter().any(|f|f == "monty-http-v1"));
    // A submodule edit changes the deploy hash, even when __init__.py is unchanged.
    std::fs::write(package.join("values.py"), "answer = 43").unwrap();
    let updated = build(&options(config.clone())).unwrap();
    assert_ne!(built.version, updated.version);
    std::fs::write(&config, r#"{"name":"shop","main":"shop/__init__.py"}"#).unwrap();
    let init = build(&options(config)).unwrap();
    assert_eq!(updated.modules, init.modules);
    assert_eq!(init.manifest.do_classes, ["shop.objects.Counter"]);
}

#[test]
fn native_fetch_returns_redirects_without_contacting_the_target() {
    run_async(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let location = format!("http://{}/secret", target.local_addr().unwrap());
        let url = format!("http://{}/redirect", redirect.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = redirect.accept().await.unwrap();
            let mut buffer = [0; 4096];
            socket.read(&mut buffer).await.unwrap();
            socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let mut worker = Worker::load_config(config(
            &format!("async def run(ctx):\n    response = await ctx.fetch('{url}')\n    return response.status"),
        )).unwrap();
        let (job, reply) = request(RequestBody::Bytes("{}".into()));
        let (entry, mut ops) = worker.turn_begin(job, None);
        let mut entry = entry.unwrap();
        assert_eq!(ops.len(), 1);
        let (id, op) = ops.pop().unwrap();
        let output = tokio::time::timeout(std::time::Duration::from_secs(5), op).await.unwrap();
        assert!(worker.turn_deliver(&mut entry, id, output).is_empty());
        assert_eq!(reply.await.unwrap().unwrap().body, b"302");
        server.await.unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(50), target.accept()).await.is_err());
    });
}
