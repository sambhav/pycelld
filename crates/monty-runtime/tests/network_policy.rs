use celld_monty::{FetchContext, FetchDecision, FetchRequest, Monty, PythonNetworkPolicy};
use celld_runtime::{
    ExecutionLimits, ExecutionMetadata, HostCall, HostReply, Invocation, Request, Response,
    Runtime, Step,
};
use serde_json::json;
use std::sync::Arc;

fn context(id: &str) -> FetchContext {
    FetchContext {
        request_url: "http://worker/run".into(),
        env: json!({"secret": "hidden"}),
        object: None,
        alarm: false,
        execution: ExecutionMetadata {
            runtime_instance_id: id.into(),
            ..Default::default()
        },
        limits: ExecutionLimits::default(),
    }
}
fn request() -> FetchRequest {
    FetchRequest {
        url: "https://api.example.com/data".parse().unwrap(),
        method: "GET".into(),
        headers: vec![("x-original".into(), "yes".into())],
        body: vec![0, 255],
    }
}
fn policy(source: &str) -> PythonNetworkPolicy {
    PythonNetworkPolicy::compile(source).unwrap()
}
fn denied(policy: &PythonNetworkPolicy) {
    assert!(matches!(
        policy.decide(&context("a"), request()),
        FetchDecision::Deny(_)
    ));
}

#[test]
fn policy_rewrites_native_bytes_and_synthesizes_responses() {
    let p = policy(
        "def policy(request, context):\n    assert request['body'] == b'\\x00\\xff'\n    assert request['host'] == 'api.example.com'\n    assert 'env' not in context\n    return {'action': 'forward', 'url': 'https://proxy.example.com/', 'method': 'POST', 'headers': [('authorization', 'host-secret')], 'body': request['body'] + b'ok'}",
    );
    let FetchDecision::Forward(out) = p.decide(&context("a"), request()) else {
        panic!("denied");
    };
    assert_eq!(out.url.as_str(), "https://proxy.example.com/");
    assert_eq!(out.method, "POST");
    assert_eq!(
        out.headers,
        [("authorization".into(), "host-secret".into())]
    );
    assert_eq!(out.body, [0, 255, b'o', b'k']);
    let p = policy(
        "def policy(request, context):\n    return {'action': 'respond', 'status': 201, 'headers': [['x-policy', 'yes']], 'body': b'\\x00\\xff'}",
    );
    let FetchDecision::Respond(out) = p.decide(&context("a"), request()) else {
        panic!("denied");
    };
    assert_eq!(out.status, 201);
    assert_eq!(out.body, [0, 255]);
}

#[test]
fn documented_policy_replaces_caller_identity_headers() {
    let p = policy(include_str!("../../../examples/network-policy/policy.py"));
    let mut input = request();
    input
        .headers
        .push(("X-Runtime-Instance".into(), "forged".into()));
    let FetchDecision::Forward(out) = p.decide(&context("trusted"), input) else {
        panic!("denied");
    };
    let identities: Vec<_> = out
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("x-runtime-instance"))
        .collect();
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].1, "trusted");
}

#[test]
fn instance_and_durable_identity_control_decisions_without_shared_globals() {
    let p = Arc::new(policy(
        "seen = []\ndef policy(request, context):\n    assert len(seen) == 0\n    seen.append(context['execution']['runtime_instance_id'])\n    if context['execution']['runtime_instance_id'] == 'allowed' and context['object'] == {'class': 'app.Counter', 'id': 'customer-a'}:\n        return {'action': 'forward'}\n    return {'action': 'deny'}",
    ));
    std::thread::scope(|scope| {
        for index in 0..16 {
            let p = p.clone();
            scope.spawn(move || {
                let mut ctx = context(if index % 2 == 0 { "allowed" } else { "denied" });
                ctx.object = Some(("app.Counter".into(), "customer-a".into()));
                assert_eq!(
                    matches!(p.decide(&ctx, request()), FetchDecision::Forward(_)),
                    index % 2 == 0
                );
            });
        }
    });
    denied(&p);
}

#[test]
fn errors_timeouts_suspensions_and_bad_results_fail_closed() {
    for source in [
        "def policy(request, context): raise ValueError('operator-secret')",
        "def policy(request, context):\n    while True: pass",
        "def policy(request, context): return open('/etc/passwd').read()",
        "def policy(request, context): return missing_host_function()",
        "def policy(request, context): return {'action': 'allow'}",
        "def policy(request, context): return {'action': 'forward', 'typo': True}",
        "def policy(request, context): return {'action': 'forward', 'url': 'file:///etc/passwd'}",
        "def policy(request, context): return {'action': 'forward', 'url': 'https://user:password@example.com/'}",
        "def policy(request, context): return {'action': 'forward', 'headers': [['x-a', 'bad\\r\\nvalue']]}",
        "def policy(request, context): return {'action': 'forward', 'method': 'GET\\r\\n'}",
        "def policy(request, context): return {'action': 'respond', 'status': 999}",
        "def policy(request, context): return {'action': 'respond', 'status': 200, 'body': b'x' * 1048577}",
    ] {
        let p = policy(source);
        let FetchDecision::Deny(message) = p.decide(&context("a"), request()) else {
            panic!("allowed {source}");
        };
        assert!(!message.contains("operator-secret"));
    }
    for source in [
        "syntax???",
        "def other(a, b): pass",
        "async def policy(a, b): pass",
        "def policy(): pass",
    ] {
        assert!(PythonNetworkPolicy::compile(source).is_err());
    }
}

#[test]
fn startup_snapshot_survives_file_replacement_and_invalid_replacement() {
    let path = std::env::temp_dir().join(format!("pycelld-policy-{}.py", std::process::id()));
    std::fs::write(
        &path,
        "def policy(request, context): return {'action': 'deny'}",
    )
    .unwrap();
    let old = PythonNetworkPolicy::load(&path).unwrap();
    std::fs::write(
        &path,
        "def policy(request, context): return {'action': 'forward'}",
    )
    .unwrap();
    denied(&old);
    let new = PythonNetworkPolicy::load(&path).unwrap();
    assert!(matches!(
        new.decide(&context("a"), request()),
        FetchDecision::Forward(_)
    ));
    std::fs::write(&path, "invalid syntax!!!").unwrap();
    assert!(PythonNetworkPolicy::load(&path).is_err());
    denied(&old);
    std::fs::write(&path, [255]).unwrap();
    assert!(PythonNetworkPolicy::load(&path).is_err());
    std::fs::write(&path, "#".repeat(1024 * 1024 + 1)).unwrap();
    assert!(PythonNetworkPolicy::load(&path).is_err());
    std::fs::remove_file(path).unwrap();
    assert!(PythonNetworkPolicy::load("/nonexistent/operator-policy.py").is_err());
}

fn invocation() -> Invocation {
    Invocation {
        request: Request {
            url: "http://worker/run".into(),
            method: "POST".into(),
            headers: vec![],
            body: b"{}".to_vec(),
        },
        object: None,
        alarm: false,
        env: json!({}),
        execution: context("authoritative").execution,
        limits: ExecutionLimits::default(),
        observer: None,
    }
}

#[test]
fn workers_cannot_forge_policy_identity_and_redirects_need_fresh_decisions() {
    let runtime = Monty::new().with_network_policy(policy("def policy(request, context):\n    assert context['execution']['runtime_instance_id'] == 'authoritative'\n    if request['host'] == 'api.example.com': return {'action': 'forward'}\n    return {'action': 'deny', 'reason': 'blocked redirect'}"));
    let program = runtime.compile("async def run(ctx):\n    ctx.execution.runtime_instance_id = 'forged'\n    ctx.env['CELLD_PYTHON_NETWORK_POLICY'] = 'allow.py'\n    response = await ctx.fetch('https://api.example.com/')\n    try:\n        await ctx.fetch('https://blocked.example.com/')\n    except RuntimeError as error:\n        return str(error)").unwrap();
    let (mut execution, step) = program.start(invocation()).unwrap();
    assert!(matches!(step, Step::Call(HostCall::Fetch(_))));
    let step = execution
        .resume(HostReply::Fetch(Response {
            status: 302,
            headers: vec![("location".into(), "https://blocked.example.com/".into())],
            body: vec![],
        }))
        .unwrap();
    let Step::Return(response) = step else {
        panic!("redirect escaped policy");
    };
    assert!(String::from_utf8_lossy(&response.body).contains("blocked redirect"));
}

#[test]
fn durable_method_cannot_replace_host_object_identity() {
    let runtime = Monty::new().with_network_policy(policy("def policy(request, context):\n    if context['object'] == {'class': 'Counter', 'id': 'customer-a'} and context['alarm'] == False:\n        return {'action': 'respond', 'status': 200, 'body': 'allowed'}\n    return {'action': 'deny'}"));
    let program = runtime.compile("class Counter:\n    def __init__(self, id: str, ctx):\n        self.ctx = ctx\n    async def run(self):\n        self.ctx.id = 'forged'\n        self.ctx.env['object'] = {'class': 'Other', 'id': 'forged'}\n        response = await self.ctx.fetch('https://api.example.com/')\n        return response.status").unwrap();
    let mut call = invocation();
    call.object = Some(celld_runtime::Object {
        class: "Counter".into(),
        id: "customer-a".into(),
    });
    call.request.body = serde_json::to_vec(&json!({"wire":"{\"Dict\":[]}", "request":{"url":"http://worker/run", "method":"POST", "headers":{}}})).unwrap();
    let (_, Step::Return(response)) = program.start(call).unwrap() else {
        panic!("policy failed");
    };
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert!(body["wire"].as_str().unwrap().contains("200"));
}

#[test]
fn existing_rust_middleware_still_has_authority_and_limits_are_retained() {
    let runtime = Monty::new()
        .with_network_policy(policy(
            "def policy(request, context): return {'action': 'forward'}",
        ))
        .with_fetch_middleware(|_, _| FetchDecision::Deny("Rust host denied".into()));
    let program = runtime.compile("async def run(ctx):\n    try: return await ctx.fetch('https://api.example.com/')\n    except RuntimeError as error: return str(error)").unwrap();
    let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
        panic!("host bypassed");
    };
    assert!(String::from_utf8_lossy(&response.body).contains("Rust host denied"));
    let mut ctx = context("a");
    ctx.limits.max_payload_bytes = 100;
    denied(&policy(
        "def policy(request, context): return {'action': 'deny'}",
    ));
    assert!(matches!(
        policy("def policy(request, context): return {'action': 'forward'}")
            .decide(&ctx, request()),
        FetchDecision::Deny(_)
    ));
}
