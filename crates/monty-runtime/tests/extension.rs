use celld_monty::Monty;
use celld_runtime::{HostCall, HostReply, Invocation, Request, Response, Runtime, Step};
use serde_json::json;

fn invocation() -> Invocation {
    Invocation {
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
fn typed_boundary_preserves_binary_buffers_and_catchable_host_errors() {
    let program = Monty.compile("async def run(ctx):\n    try:\n        await ctx.sleep(-1)\n    except RuntimeError:\n        return await ctx.fetch('http://test', method='POST', body=b'abc\\x00\\xff')").unwrap();
    let (mut execution, step) = program.start(invocation()).unwrap();
    let Step::Call(HostCall::Fetch(request)) = step else {
        panic!("expected typed fetch")
    };
    assert_eq!(request.body, b"abc\x00\xff");
    let body = vec![0, 255, 42];
    let step = execution
        .resume(HostReply::Fetch(Response {
            status: 201,
            headers: vec![],
            body,
        }))
        .unwrap();
    let Step::Return(response) = step else {
        panic!("expected response")
    };
    assert_eq!(response.status, 201);
    assert_eq!(response.body, [0, 255, 42]);
}

#[test]
fn errors_and_type_definitions_belong_to_the_runtime() {
    let program = Monty
        .compile("def run(): raise ValueError('boom')")
        .unwrap();
    let error = program.start(invocation()).err().unwrap();
    assert_eq!(error.code, "ValueError");
    let response = program.error_response(error, true);
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["error"]["code"], "ValueError");
    assert_eq!(response.status, 200);
    assert!(Monty.types().contains("class Context:"));
}
