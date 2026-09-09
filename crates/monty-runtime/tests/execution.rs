use celld_monty::{FetchDecision, Monty};
use celld_runtime::{
    ExecutionLimits, ExecutionMetadata, HostCall, HostReply, Invocation, Request, Runtime, Step,
};
use serde_json::json;

fn invocation(limits: ExecutionLimits) -> Invocation {
    Invocation {
        observer: None,
        execution: ExecutionMetadata {
            application: celld_runtime::ApplicationIdentity {
                project_id: "project-1".into(),
                application_id: "app-1".into(),
                stage: Some("production".into()),
                ..Default::default()
            },
            worker_id: "trusted-worker".into(),
            deployment_id: "version-1".into(),
            runtime_instance_id: "slot-1".into(),
            invocation_id: "call-1".into(),
            root_invocation_id: "call-1".into(),
            ..Default::default()
        },
        limits,
        request: Request {
            url: "http://test/run".into(),
            method: "POST".into(),
            headers: vec![],
            body: b"{}".to_vec(),
        },
        object: None,
        alarm: false,
        env: json!({}),
    }
}
#[test]
fn python_mutations_cannot_change_policy_identity_or_limits() {
    let runtime = Monty::new().with_fetch_middleware(|context, _| {
        assert_eq!(context.execution.worker_id, "trusted-worker");
        assert_eq!(context.execution.invocation_id, "call-1");
        assert_eq!(context.execution.application.project_id, "project-1");
        assert_eq!(context.limits.max_operations, 2);
        FetchDecision::Deny("checked authoritative metadata".into())
    });
    let program = runtime.compile("async def run(ctx):\n    ctx.execution.application.project_id = 'spoofed'\n    ctx.execution.worker_id = 'spoofed'\n    ctx.execution.invocation_id = 'spoofed'\n    ctx.limits.max_operations = 999999\n    try:\n        await ctx.fetch('https://example.test')\n    except RuntimeError:\n        ctx.now()\n        ctx.now()\n").unwrap();
    let (mut execution, step) = program
        .start(invocation(ExecutionLimits {
            max_operations: 2,
            ..Default::default()
        }))
        .unwrap();
    assert!(matches!(step, Step::Call(HostCall::Now)));
    let error = execution
        .resume(HostReply::Timestamp(Some(0)))
        .err()
        .unwrap();
    assert!(error.message.contains("host call limit"), "{error}");
}
#[test]
fn headers_results_and_host_replies_obey_payload_ceiling() {
    let limits = ExecutionLimits {
        max_payload_bytes: 128,
        ..Default::default()
    };
    let program = Monty::new().compile("def run(): return 'x' * 129").unwrap();
    assert!(program.start(invocation(limits)).is_err());
    let mut call = invocation(limits);
    call.request.headers.push(("large".into(), "x".repeat(128)));
    assert!(
        program
            .start(call)
            .err()
            .unwrap()
            .message
            .contains("payload")
    );
    let program = Monty::new()
        .compile("def run(ctx): return ctx.now()")
        .unwrap();
    let (mut execution, _) = program.start(invocation(limits)).unwrap();
    assert!(
        execution
            .resume(HostReply::Error("x".repeat(129)))
            .err()
            .unwrap()
            .message
            .contains("payload")
    );
}
#[test]
fn cpu_exhaustion_is_fatal_even_if_python_catches_exceptions() {
    let program = Monty::new().compile("def run():\n    try:\n        while True:\n            pass\n    except BaseException:\n        return 'escaped'\n").unwrap();
    let error = program
        .start(invocation(ExecutionLimits {
            cpu_ms: 1,
            ..Default::default()
        }))
        .err()
        .unwrap();
    assert!(error.message.contains("time limit"), "{error}");
}
#[test]
fn invalid_host_limits_fail_before_interpreter_entry() {
    let program = Monty::new().compile("def run(): return 'hello'").unwrap();
    assert!(
        program
            .start(invocation(ExecutionLimits {
                cpu_ms: 0,
                ..Default::default()
            }))
            .err()
            .unwrap()
            .message
            .contains("cpu_ms")
    );
}
#[test]
fn cpu_budget_survives_host_suspensions() {
    let program = Monty::new().compile("def run(ctx):\n    while True:\n        for i in range(100):\n            value = i * i\n        ctx.now()\n").unwrap();
    let (mut execution, mut step) = program
        .start(invocation(ExecutionLimits {
            cpu_ms: 10,
            max_operations: 1_000_000,
            ..Default::default()
        }))
        .unwrap();
    loop {
        assert!(matches!(step, Step::Call(HostCall::Now)));
        match execution.resume(HostReply::Timestamp(Some(0))) {
            Ok(next) => step = next,
            Err(error) => {
                assert!(error.message.contains("time limit"), "{error}");
                break;
            }
        }
    }
}

#[test]
fn suspension_wait_does_not_consume_cpu_and_each_invocation_gets_a_new_budget() {
    let program = Monty::new()
        .compile("def run(ctx):\n    ctx.now()\n    return 'done'\n")
        .unwrap();
    let limits = ExecutionLimits {
        cpu_ms: 100,
        max_operations: 1,
        ..Default::default()
    };
    let (mut one, _) = program.start(invocation(limits)).unwrap();
    let (mut two, _) = program.start(invocation(limits)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(matches!(
        one.resume(HostReply::Timestamp(Some(0))).unwrap(),
        Step::Return(_)
    ));
    assert!(matches!(
        two.resume(HostReply::Timestamp(Some(0))).unwrap(),
        Step::Return(_)
    ));
}

#[test]
fn recursion_is_configurable_and_unsupported_memory_limits_fail_before_entry() {
    let program = Monty::new().compile("def recurse(n):\n    if n == 0: return 1\n    return recurse(n - 1)\ndef run(): return recurse(25)\n").unwrap();
    assert!(
        program
            .start(invocation(ExecutionLimits::default()))
            .is_ok()
    );
    let error = program
        .start(invocation(ExecutionLimits {
            max_recursion_depth: 15,
            ..Default::default()
        }))
        .err()
        .unwrap();
    assert!(error.message.contains("recursion"), "{error}");
    let error = program
        .start(invocation(ExecutionLimits {
            max_memory_bytes: Some(1024 * 1024),
            ..Default::default()
        }))
        .err()
        .unwrap();
    assert!(error.message.contains("allocator"), "{error}");
}
