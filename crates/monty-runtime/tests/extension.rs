use celld_monty::{ExcType, Monty, PythonError, PythonValue};
use celld_runtime::{HostCall, HostReply, Invocation, Request, Response, Runtime, Step};
use serde_json::json;

fn invocation() -> Invocation {
    Invocation {
        execution: Default::default(),
        limits: Default::default(),
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
    let program = Monty::new().with_fetch_middleware(|_, request| celld_monty::FetchDecision::Forward(request)).compile("async def run(ctx):\n    try:\n        await ctx.sleep(-1)\n    except RuntimeError:\n        return await ctx.fetch('http://test', method='POST', body=b'abc\\x00\\xff')").unwrap();
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

fn package(entry: &str, sources: &[(&str, bool, &str)]) -> String {
    let modules: serde_json::Map<String, serde_json::Value> = sources
        .iter()
        .map(|(name, package, source)| ((*name).into(), json!({"source":source,"package":package})))
        .collect();
    format!(
        "# celld:python-package-v1\n{}",
        json!({"entry":entry,"modules":modules})
    )
}

#[test]
fn extension_modules_have_independent_names_aliases_and_parent_packages() {
    use celld_monty::PythonModule;
    let runtime = Monty::new()
        .with_module(
            PythonModule::new("acme.text")
                .with_function("def format(value: str) -> str: ...", |args| {
                    let [PythonValue::String(value)] = args.as_slice() else {
                        panic!("expected string")
                    };
                    Ok(PythonValue::String(value.to_uppercase()))
                })
                .unwrap(),
        )
        .unwrap()
        .with_module(
            PythonModule::new("acme.other")
                .with_function("def format(value: str) -> str: ...", |args| {
                    Ok(args[0].clone())
                })
                .unwrap(),
        )
        .unwrap();
    let program = runtime.compile("import acme.text\nimport acme.text as text\nfrom acme.other import format as other\nfrom celld import Context as Ctx\ndef run(ctx: Ctx):\n    return [acme.text.format('one'), text.format('two'), other('three'), ctx.env]").unwrap();
    let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
        panic!("not a response")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&response.body).unwrap(),
        json!(["ONE", "TWO", "three", {}])
    );
    let types = runtime.type_files();
    assert!(types["acme/text.pyi"].contains("def format"));
    assert!(types.contains_key("acme/__init__.pyi"));
    assert!(types["celld.pyi"].contains("class Context:"));
    assert!(!runtime.types().contains("def format"));
    assert!(runtime.with_module(PythonModule::new("acme.text")).is_err());
    assert!(
        Monty::new()
            .with_module(PythonModule::new("../../bad"))
            .is_err()
    );
    assert!(Monty::new().with_module(PythonModule::new("json")).is_err());
}

#[test]
fn package_imports_preserve_globals_closures_and_lazy_module_initialization() {
    let source = package(
        "app",
        &[
            (
                "app",
                true,
                "from .left import compute as run\n__all__ = ['run']",
            ),
            (
                "app.left",
                false,
                "from . import right\ncount = 1\ndef compute():\n    global count\n    count += 1\n    right.count = 20\n    values = [count for count in range(3)]\n    def local(count): return count + 1\n    def outer():\n        count = 5\n        def inner():\n            nonlocal count\n            count += 1\n            return count\n        return inner()\n    return [count, right.read(), values, local(10), outer(), (lambda count: count * 2)(7)]",
            ),
            (
                "app.right",
                false,
                "from . import left\ncount = 100\ndef read(): return count",
            ),
            ("app.unused", false, "raise ValueError('must remain lazy')"),
        ],
    );
    let program = Monty::new().compile(&source).unwrap();
    for _ in 0..2 {
        let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
            panic!("not a response")
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&response.body).unwrap(),
            json!([2, 20, [0, 1, 2], 11, 6, 14])
        );
    }
    let mut hidden = invocation();
    hidden.request.url = "http://test/compute".into();
    let (_, Step::Return(response)) = program.start(hidden).unwrap() else {
        panic!("not a response")
    };
    assert_eq!(response.status, 404);
}

#[test]
fn package_durable_classes_keep_qualified_identity_and_typed_context() {
    let source = package(
        "shop",
        &[
            ("shop", true, "from .handlers import run\n__all__ = ['run']"),
            (
                "shop.handlers",
                false,
                "from celld import Context\nfrom .objects import Counter\ndef run(ctx: Context): return Counter('one', ctx).get()",
            ),
            (
                "shop.objects",
                false,
                "from celld import Context\nfrom .value import Value\nclass Counter:\n    def __init__(self, id: str, ctx: Context):\n        self.id = id\n        self._ctx = ctx\n    def get(self): return Value(self.id, self._ctx.env['GREETING'])",
            ),
            (
                "shop.value",
                false,
                "from dataclasses import dataclass\n@dataclass\nclass Value:\n    id: str\n    greeting: str",
            ),
        ],
    );
    let program = Monty::new().compile(&source).unwrap();
    assert_eq!(program.classes(), ["shop.objects.Counter"]);
    let mut call = invocation();
    call.env = json!({"GREETING":"hello"});
    let (
        mut execution,
        Step::Call(HostCall::CallObject {
            object,
            method,
            body,
        }),
    ) = program.start(call).unwrap()
    else {
        panic!("expected object call")
    };
    assert_eq!(object.class, "shop.objects.Counter");
    let mut call = invocation();
    call.env = json!({"GREETING":"hello"});
    call.object = Some(object);
    call.request.url = format!("http://test/{method}");
    call.request.body = body;
    let (_, Step::Return(response)) = program.start(call).unwrap() else {
        panic!("expected object return")
    };
    let Step::Return(response) = execution.resume(HostReply::Object(response)).unwrap() else {
        panic!("expected HTTP return")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&response.body).unwrap(),
        json!({"id":"one", "greeting":"hello"})
    );
}

#[test]
fn modules_registered_in_any_order_support_relative_class_imports() {
    use celld_monty::PythonModule;
    let runtime = Monty::new()
        .with_module(PythonModule::new("acme.labels").with_python(
            "from .native import prefix\nfrom dataclasses import dataclass\n@dataclass\nclass Label:\n    text: str\ndef label(value: str) -> Label: return Label(prefix(value))",
            "from dataclasses import dataclass\n@dataclass\nclass Label:\n    text: str\ndef label(value: str) -> Label: ...",
        ).unwrap()).unwrap()
        .with_module(PythonModule::new("acme.native").with_function("def prefix(value: str) -> str: ...", |args| {
            let [PythonValue::String(value)] = args.as_slice() else { panic!("expected string") };
            Ok(PythonValue::String(format!("hello {value}")))
        }).unwrap()).unwrap();
    let program = runtime.compile("def run():\n    from acme.labels import label as make_label\n    return make_label('world')").unwrap();
    let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
        panic!("not a response")
    };
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&response.body).unwrap(),
        json!({"text":"hello world"})
    );
}

#[test]
fn qualified_imports_preserve_dataclasses_named_like_builtin_response() {
    let source = package(
        "app",
        &[
            (
                "app",
                true,
                "from __future__ import annotations\nfrom .models import Response as Payload\ndef run() -> Payload:\n    return Payload(42)",
            ),
            (
                "app.models",
                false,
                "from dataclasses import dataclass\n@dataclass\nclass Response:\n    value: int",
            ),
        ],
    );
    let program = Monty::new().compile(&source).unwrap();
    let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
        panic!("expected response")
    };
    assert_eq!(response.status, 200);
    assert_eq!(response.body, br#"{"value":42}"#);
}

#[test]
fn fetch_is_denied_by_default_and_python_can_catch_it() {
    let program = Monty::new().compile("async def run(ctx):\n    try: await ctx.fetch('https://example.com')\n    except RuntimeError as error: return str(error)").unwrap();
    let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
        panic!("denied fetch escaped to the host")
    };
    assert!(String::from_utf8(response.body).unwrap().contains("outbound HTTP is disabled"));
}

#[test]
fn middleware_receives_context_and_rewrites_before_the_next_policy() {
    use celld_monty::FetchDecision;
    let runtime = Monty::new()
        .with_fetch_middleware(|ctx, mut request| {
            assert_eq!(ctx.request_url, "http://test/run");
            assert_eq!(ctx.env["TENANT"], "acme");
            assert!(ctx.object.is_none());
            assert!(!ctx.alarm);
            assert_eq!(request.url.host_str(), Some("example.com"));
            assert_eq!(request.body, b"\x00\xff");
            request.url.set_host(Some("gateway.example")).unwrap();
            request.headers.push(("x-tenant".into(), "acme".into()));
            FetchDecision::Forward(request)
        })
        .with_fetch_middleware(|_, request| {
            assert_eq!(request.url.host_str(), Some("gateway.example"));
            assert!(request.headers.contains(&("x-tenant".into(), "acme".into())));
            FetchDecision::Forward(request)
        });
    let program = runtime.compile("async def run(ctx): return await ctx.fetch('https://EXAMPLE.com/path', method='POST', body=b'\\x00\\xff')").unwrap().fork();
    let mut call = invocation();
    call.env = json!({"TENANT":"acme"});
    let (_, Step::Call(HostCall::Fetch(request))) = program.start(call).unwrap() else {
        panic!("expected approved fetch")
    };
    assert_eq!(request.url, "https://gateway.example/path");
    assert_eq!(request.method, "POST");
    assert_eq!(request.body, [0, 255]);
}

#[test]
fn middleware_can_deny_after_rewrite_and_short_circuits() {
    use celld_monty::FetchDecision;
    let runtime = Monty::new()
        .with_fetch_middleware(|_, mut request| {
            request.url.set_host(Some("blocked.example")).unwrap();
            FetchDecision::Forward(request)
        })
        .with_fetch_middleware(|_, request| {
            assert_eq!(request.url.host_str(), Some("blocked.example"));
            FetchDecision::Deny("tenant policy denied this call".into())
        })
        .with_fetch_middleware(|_, _| panic!("middleware after deny must not run"));
    let program = runtime.compile("async def run(ctx):\n    try: await ctx.fetch('https://example.com')\n    except RuntimeError as error: return str(error)").unwrap();
    let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
        panic!("denied fetch escaped to transport")
    };
    assert_eq!(response.body, b"tenant policy denied this call");
}

#[test]
fn synthetic_fetch_responses_preserve_bytes_and_do_not_recurse() {
    use celld_monty::FetchDecision;
    let runtime = Monty::new()
        .with_fetch_middleware(|_, _| FetchDecision::Respond(Response {
            status: 201, headers: vec![("x-source".into(), "middleware".into())],
            body: vec![0, 255],
        }))
        .with_fetch_middleware(|_, _| panic!("middleware after response must not run"));
    let program = runtime.compile("async def run(ctx):\n    for i in range(1000):\n        response = await ctx.fetch('https://example.com')\n        assert response.headers['x-source'] == 'middleware'\n    return response").unwrap();
    let mut call = invocation();
    call.limits.cpu_ms = 1000; // This checks stack safety, not execution speed.
    let (_, Step::Return(response)) = program.start(call).unwrap() else {
        panic!("synthetic fetch escaped to transport")
    };
    assert_eq!(response.status, 201);
    assert_eq!(response.body, [0, 255]);
}

#[test]
fn middleware_rewrites_cannot_escape_http_or_body_limits() {
    use celld_monty::FetchDecision;
    for oversized in [false, true] {
        let runtime = Monty::new().with_fetch_middleware(move |_, mut request| {
            if oversized { request.body = vec![0; 1024 * 1024 + 1]; }
            else { request.url = "file:///etc/passwd".parse().unwrap(); }
            FetchDecision::Forward(request)
        });
        let program = runtime.compile("async def run(ctx):\n    try: await ctx.fetch('https://example.com')\n    except RuntimeError: return 'blocked'").unwrap();
        let (_, Step::Return(response)) = program.start(invocation()).unwrap() else {
            panic!("invalid rewritten request escaped to transport")
        };
        assert_eq!(response.body, b"blocked");
    }
}

#[test]
fn fetch_policy_applies_in_durable_methods_and_injected_helpers() {
    use celld_monty::FetchDecision;
    let runtime = Monty::new().with_python(
        "async def upstream(ctx): return await ctx.fetch('https://example.com')",
        "from celld import Context, Response\nasync def upstream(ctx: Context) -> Response: ...",
    ).unwrap().with_fetch_middleware(|ctx, _| {
        let (class, id) = ctx.object.as_ref().unwrap();
        assert_eq!(class, "Counter");
        assert_eq!(id, "tenant-1");
        FetchDecision::Deny("blocked in durable object".into())
    });
    let program = runtime.compile("from celld import upstream\nclass Counter:\n    def __init__(self, id, ctx): self._ctx = ctx\n    async def run(self):\n        try: await upstream(self._ctx)\n        except RuntimeError as error: return str(error)\ndef run(): return 0").unwrap();
    let mut call = invocation();
    call.object = Some(celld_runtime::Object { class: "Counter".into(), id: "tenant-1".into() });
    call.request.body = serde_json::to_vec(&json!({"wire":"{\"Dict\":[]}", "request": {}})).unwrap();
    let (_, Step::Return(response)) = program.start(call).unwrap() else {
        panic!("durable fetch escaped to transport")
    };
    assert!(String::from_utf8(response.body).unwrap().contains("blocked in durable object"));
}


#[test]
fn python_errors_after_synthetic_responses_keep_their_exception_type() {
    let runtime = Monty::new().with_fetch_middleware(|_, _| {
        celld_monty::FetchDecision::Respond(Response { status: 200, headers: vec![], body: vec![] })
    });
    let program = runtime.compile("async def run(ctx):\n    await ctx.fetch('https://example.com')\n    raise ValueError('after fetch')").unwrap();
    let error = program.start(invocation()).err().unwrap();
    assert_eq!(error.code, "ValueError");
    assert_eq!(error.message, "after fetch");
}
