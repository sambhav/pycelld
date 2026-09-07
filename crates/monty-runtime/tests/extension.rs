use celld_monty::{ExcType, Monty, PythonError, PythonValue};
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
    let program = Monty::new().compile("async def run(ctx):\n    try:\n        await ctx.sleep(-1)\n    except RuntimeError:\n        return await ctx.fetch('http://test', method='POST', body=b'abc\\x00\\xff')").unwrap();
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
    let program = Monty::new()
        .compile("def run(): raise ValueError('boom')")
        .unwrap();
    let error = program.start(invocation()).err().unwrap();
    assert_eq!(error.code, "ValueError");
    let response = program.error_response(error, true);
    let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["error"]["code"], "ValueError");
    assert_eq!(response.status, 200);
    assert!(Monty::new().types().contains("class Context:"));
}

#[test]
fn injected_functions_bind_keywords_defaults_and_return_native_values() {
    let runtime = Monty::new()
        .with_function(
            "def repeat(data: bytes, *, count: int = 2) -> bytes: ...",
            |args| match args.as_slice() {
                [PythonValue::Bytes(data), PythonValue::Int(count)] if (0..=3).contains(count) => {
                    Ok(PythonValue::Bytes(data.repeat(*count as usize)))
                }
                _ => Err(PythonError::new(
                    ExcType::ValueError,
                    Some("invalid repeat arguments".into()),
                )),
            },
        )
        .unwrap();
    assert!(
        runtime
            .types()
            .contains("def repeat(data: bytes, *, count: int = 2) -> bytes: ...")
    );
    let source = "from celld import repeat\ndef run():\n    try:\n        repeat(b'x', count=-1)\n    except ValueError:\n        return repeat(data=b'\\xff' * 300000)";
    let program = runtime.compile(source).unwrap().fork();
    let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
        panic!("not a response")
    };
    assert_eq!(response.body, vec![255; 600000]);
    let mut hidden = invocation();
    hidden.request.url = "http://test/repeat".into();
    let (_, Step::Return(response)) = program.start(hidden).unwrap() else {
        panic!("not a response")
    };
    assert_eq!(response.status, 404);
}

#[test]
fn injected_classes_work_in_handlers_and_durable_objects_with_typed_context() {
    let helpers = "from dataclasses import dataclass\nfrom celld import Context\n@dataclass\nclass Label:\n    text: str\nclass Labels:\n    def __init__(self, ctx: Context):\n        self._ctx = ctx\n    def next(self) -> Label:\n        return Label(self._ctx.uuid())\ndef decorate(text: str) -> str: return '[' + text + ']'";
    let types = "from dataclasses import dataclass\nfrom celld import Context\n@dataclass\nclass Label:\n    text: str\nclass Labels:\n    def __init__(self, ctx: Context) -> None: ...\n    def next(self) -> Label: ...\ndef decorate(text: str) -> str: ...";
    let runtime = Monty::new().with_python(helpers, types).unwrap();
    let program = runtime.compile("from celld import Labels, decorate, Context\nclass Counter:\n    def __init__(self, id: str, ctx: Context):\n        self._ctx = ctx\n    def label(self): return Labels(self._ctx).next()\ndef run(ctx: Context): return Labels(ctx).next()").unwrap();
    assert_eq!(program.classes(), ["Counter"]);
    for durable in [false, true] {
        let mut call = invocation();
        if durable {
            call.request.url = "http://test/label".into();
            call.object = Some(celld_runtime::Object {
                class: "Counter".into(),
                id: "test".into(),
            });
            call.request.body = serde_json::to_vec(&json!({"wire":"{\"Dict\":[]}", "request": {"url": "http://test/label", "method": "POST", "headers": {}}})).unwrap();
        }
        let (mut execution, Step::Call(HostCall::Uuid)) = program.start(call).unwrap() else {
            panic!("expected uuid")
        };
        let Step::Return(response) = execution
            .resume(HostReply::Value(json!("native-id")))
            .unwrap()
        else {
            panic!("not a response")
        };
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        if durable {
            assert!(body["wire"].as_str().unwrap().contains("native-id"));
        } else {
            assert_eq!(body, json!({"text":"native-id"}));
        }
    }
    assert!(runtime.types().contains("class Labels:"));
    assert!(runtime.types().contains("class Context:"));
}

#[test]
fn extension_registration_rejects_collisions_and_incomplete_interfaces() {
    for signature in [
        "def f(x): ...",
        "def f(x: int): ...",
        "async def f() -> str: ...",
        "def f(*args: int) -> int: ...",
        "def f() -> int: return 1",
        "def Context() -> str: ...",
        "def _private() -> str: ...",
    ] {
        assert!(
            Monty::new()
                .with_function(signature, |_| Ok(PythonValue::None))
                .is_err(),
            "{signature}"
        );
    }
    let runtime = Monty::new()
        .with_function("def named() -> int: ...", |_| Ok(PythonValue::Int(1)))
        .unwrap();
    assert!(
        runtime
            .clone()
            .with_function("def named() -> int: ...", |_| Ok(PythonValue::Int(2)))
            .is_err()
    );
    assert!(
        runtime
            .with_python("def named(): return 2", "def named() -> int: ...")
            .is_err()
    );
    assert!(
        Monty::new()
            .with_python("class Context: pass", "class Context: ...")
            .is_err()
    );
    assert!(
        Monty::new()
            .with_python("def helper(): return 1", "def different() -> int: ...")
            .is_err()
    );
    assert!(
        Monty::new()
            .compile("from celld import unavailable\ndef run(): return unavailable()")
            .is_err()
    );
}

#[test]
fn injected_functions_obey_result_and_invocation_limits() {
    let runtime = Monty::new()
        .with_function("def large() -> bytes: ...", |_| {
            Ok(PythonValue::Bytes(vec![0; 1024 * 1024 + 1]))
        })
        .unwrap();
    let program = runtime.compile("from celld import large\ndef run():\n    try: return large()\n    except ValueError: return 'bounded'").unwrap();
    let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
        panic!("not a response")
    };
    assert_eq!(response.body, b"bounded");
    let runtime = Monty::new()
        .with_function("def tick() -> None: ...", |_| Ok(PythonValue::None))
        .unwrap();
    let program = runtime
        .compile("from celld import tick\ndef run():\n    for i in range(10001): tick()")
        .unwrap();
    let error = program.start(invocation()).err().unwrap();
    assert!(
        error.message.contains("host call limit") || error.code == "TimeoutError",
        "{error}"
    );
}

#[test]
fn native_callbacks_preserve_dataclass_fields() {
    let runtime = Monty::new()
        .with_python(
            "from dataclasses import dataclass\n@dataclass\nclass Record:\n    name: str\n    payload: bytes",
            "from dataclasses import dataclass\n@dataclass\nclass Record:\n    name: str\n    payload: bytes",
        ).unwrap()
        .with_function("def identity(record: Record) -> Record: ...", |mut args| Ok(args.remove(0))).unwrap();
    let program = runtime.compile("from celld import Record, identity\ndef run():\n    record = identity(Record('test', b'\\x00\\xff'))\n    return {'name': record.name, 'size': len(record.payload)}").unwrap();
    let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
        panic!("not a response")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&response.body).unwrap(),
        json!({"name":"test", "size":2})
    );
}
