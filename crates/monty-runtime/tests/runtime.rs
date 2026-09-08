use crate::{
    Session,
    exports::{Module, durable_classes},
};
use serde_json::{Value, json};

fn call(source: &str, name: &str, args: Value) -> crate::value::HttpResponse {
    let module = Module::compile(source).unwrap();
    let (mut session, event) =
        Session::start(module.get(name).unwrap(), &args, &json!({})).unwrap();
    assert_eq!(event["done"], true);
    assert!(event.get("wire").is_none());
    session.take_response().unwrap()
}
#[test]
fn response_types_are_native_and_unwrapped() {
    for (source, status, body) in [
        ("def run(): return 'hello'", 200, json!("hello")),
        ("def run(): return None", 204, Value::Null),
        ("def run(): return b'abc'", 200, json!([97, 98, 99])),
        (
            "def run(): return {'ok': True, 'items': (1,2)}",
            200,
            json!("{\"items\":[1,2],\"ok\":true}"),
        ),
        (
            "from dataclasses import dataclass\n@dataclass\nclass Item:\n    name: str\n    count: int\ndef run() -> Item: return Item('hello',2)",
            200,
            json!("{\"count\":2,\"name\":\"hello\"}"),
        ),
        (
            "from celld import Response\ndef run(): return Response('created',status=201,headers={'x-test':'yes'})",
            201,
            json!("created"),
        ),
    ] {
        let event = call(source, "run", json!({}));
        assert_eq!(event.status, status, "{source}");
        let expected = match body {
            Value::Null => vec![],
            Value::String(s) => s.into_bytes(),
            b => serde_json::from_value::<Vec<u8>>(b).unwrap(),
        };
        assert_eq!(event.body, expected, "{source}");
    }
}
#[test]
fn explicit_exports_and_argument_validation() {
    let module = Module::compile("__all__=['add']\ndef add(left:int, *,right:int=1,ctx=None): return left+right\ndef hidden():return 0").unwrap();
    assert!(module.get("hidden").is_none());
    for args in [
        json!({}),
        json!({"left":true}),
        json!({"left":2,"ctx":{}}),
        json!([]),
    ] {
        assert_eq!(
            Session::start(module.get("add").unwrap(), &args, &json!({}))
                .err()
                .unwrap()
                .status,
            400
        );
    }
    let (mut session, _) =
        Session::start(module.get("add").unwrap(), &json!({"left":2}), &json!({})).unwrap();
    assert_eq!(body(&mut session), "3");
    for source in [
        "__all__=['_hidden']\ndef _hidden():pass",
        "__all__=[]\n__all__.append('x')\ndef x():pass",
        "def x(*args):pass",
    ] {
        assert!(Module::compile(source).is_err());
    }
}
#[test]
fn exceptions_remain_exceptions_and_bad_json_is_rejected() {
    let module = Module::compile("def run(): raise ValueError('boom')").unwrap();
    let error = Session::start(module.get("run").unwrap(), &json!({}), &json!({}))
        .err()
        .unwrap();
    assert_eq!(error.status, 500);
    assert_eq!(error.code, "ValueError");
    assert_eq!(error.message, "boom");
    let module = Module::compile("def run(): return {1:'bad'}").unwrap();
    assert!(Session::start(module.get("run").unwrap(), &json!({}), &json!({})).is_err());
}
#[test]
fn iterators_are_rejected_without_consuming_them() {
    for source in [
        "def run():return iter(range(3))",
        "class Items:\n    def __iter__(self):return self\n    def __next__(self):raise ValueError('must not consume')\ndef run():return Items()",
    ] {
        let module = Module::compile(source).unwrap();
        let error = Session::start(module.get("run").unwrap(), &json!({}), &json!({}))
            .err()
            .unwrap();
        assert_ne!(error.code, "ValueError");
    }
    assert!(Module::compile("def run():yield 1").is_err());
}
#[test]
fn direct_class_calls_suspend_as_rpc_and_constructor_receives_owner_context() {
    let source = include_str!("../../../examples/monty/worker.py");
    assert_eq!(durable_classes(source).unwrap(), vec!["Counter"]);
    let module = Module::compile(source).unwrap();
    let (_, event) = Session::start(
        module.get("increment").unwrap(),
        &json!({"id":"cart/東京"}),
        &json!({}),
    )
    .unwrap();
    assert_eq!(event["operation"], "object.call");
    assert_eq!(event["args"][0], "Counter");
    assert_eq!(event["args"][1], "cart/東京");
    assert_eq!(event["args"][2], "increment");
    let module = Module::compile_class(source, "Counter").unwrap();
    let (mut session, event) = Session::start(
        module.get("increment").unwrap(),
        &json!({"amount":2}),
        &json!({"id":"cart/東京"}),
    )
    .unwrap();
    assert_eq!(event["operation"], "storage.transaction_begin");
    assert_eq!(
        session.resume(json!({"result":null})).unwrap()["operation"],
        "storage.get"
    );
    assert_eq!(
        session
            .resume(json!({"result":{"found":false,"value":null}}))
            .unwrap()["args"],
        json!(["count", 2])
    );
    assert_eq!(
        session.resume(json!({"result":null})).unwrap()["operation"],
        "storage.transaction_commit"
    );
    let result = session.resume(json!({"result":null})).unwrap();
    let value = serde_json::from_str(result["wire"].as_str().unwrap()).unwrap();
    assert_eq!(
        crate::value::to_json(&value).unwrap(),
        json!({"id":"cart/東京","value":2})
    );
    assert!(result.get("response").is_none());
}
#[test]
fn host_errors_can_be_caught_and_transactions_rollback() {
    let source = "def run(ctx):\n    try:\n        with ctx.storage.transaction() as tx:\n            tx.set('x',1)\n            raise ValueError('rollback')\n    except ValueError:\n        return True";
    let module = Module::compile(source).unwrap();
    let (mut session, _) =
        Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    assert_eq!(
        session.resume(json!({"result":null})).unwrap()["operation"],
        "storage.put"
    );
    assert_eq!(
        session.resume(json!({"result":null})).unwrap()["operation"],
        "storage.transaction_rollback"
    );
    session.resume(json!({"result":null})).unwrap();
    assert_eq!(body(&mut session), "true");
}
#[test]
fn context_types_match_runtime_and_alarm_accepts_timedelta() {
    let module = Module::compile("from celld import Context\nfrom datetime import timedelta\ndef run(ctx: Context):ctx.alarms.set(timedelta(seconds=2))").unwrap();
    let (mut session, event) =
        Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    assert_eq!(event["operation"], "storage.set_alarm");
    assert_eq!(event["args"], json!([2.0]));
    assert_eq!(
        session.resume(json!({"result":null})).unwrap()["done"],
        true
    );
}
#[test]
fn cpu_budget_stops_busy_handlers() {
    let module = Module::compile("def run():\n    while True: pass").unwrap();
    assert!(Session::start(module.get("run").unwrap(), &json!({}), &json!({})).is_err());
}

#[test]
fn remote_dataclass_return_retains_typed_field_access() {
    let source = "from dataclasses import dataclass\n@dataclass\nclass Count:\n    value:int\nclass Counter:\n    def __init__(self,id,ctx):self.id=id\n    def read(self)->Count:return Count(42)\ndef run(ctx):return Counter('one',ctx).read().value";
    let object = Module::compile_class(source, "Counter").unwrap();
    let (_, event) = Session::start(
        object.get("read").unwrap(),
        &json!({}),
        &json!({"id":"one"}),
    )
    .unwrap();
    let module = Module::compile(source).unwrap();
    let (mut session, _) =
        Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    session.resume(json!({"wire":event["wire"]})).unwrap();
    assert_eq!(body(&mut session), "42");
}

#[test]
fn large_integers_and_nested_dataclasses_do_not_lose_precision() {
    let source = "from dataclasses import dataclass\n@dataclass\nclass Inner:\n    value:int\n@dataclass\nclass Outer:\n    inner:Inner\ndef run(value:int):return Outer(Inner(value))";
    let number = "340282366920938463463374607431768211457";
    let args: Value = serde_json::from_str(&format!("{{\"value\":{number}}}")).unwrap();
    let event = call(source, "run", args);
    assert_eq!(
        String::from_utf8(event.body).unwrap(),
        format!("{{\"inner\":{{\"value\":{number}}}}}")
    );
}

#[test]
fn remote_python_exceptions_can_be_caught_by_type() {
    let source = "class Counter:\n    def __init__(self,id,ctx):pass\n    def fail(self):raise ValueError('boom')\ndef run(ctx):\n    try:return Counter('one',ctx).fail()\n    except ValueError:return 'caught'";
    let module = Module::compile(source).unwrap();
    let (mut session, _) =
        Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    let event = session
        .resume(json!({"error":{"code":"ValueError","message":"boom"}}))
        .unwrap();
    assert_eq!(event["done"], true);
    assert_eq!(body(&mut session), "caught");
}

#[test]
fn datetime_results_use_iso_strings() {
    let event = call(
        "from datetime import datetime, timezone\ndef run(): return datetime(2026,9,7,1,2,3,tzinfo=timezone.utc)",
        "run",
        json!({}),
    );
    assert_eq!(
        String::from_utf8(event.body).unwrap(),
        "\"2026-09-07T01:02:03+00:00\""
    );
}

#[test]
fn context_clock_and_alarm_read_return_aware_datetimes() {
    for source in [
        "def run(ctx):return ctx.now()",
        "def run(ctx):return ctx.alarms.get()",
    ] {
        let module = Module::compile(source).unwrap();
        let (mut session, _) =
            Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
        let event = session
            .resume(crate::value::timestamp_reply(3000).unwrap())
            .unwrap();
        assert_eq!(event["done"], true);
        assert_eq!(body(&mut session), "\"1970-01-01T00:00:03+00:00\"");
    }
}

#[test]
fn constructors_have_the_same_signature_on_handles_and_owned_instances() {
    for parameters in ["self,ctx,id", "self,id,*,ctx", "self,id,ctx=None"] {
        let source = format!(
            "class Counter:\n    def __init__({parameters}):pass\n    def read(self):return 1"
        );
        assert!(Module::compile_class(&source, "Counter").is_err());
    }
}

#[test]
fn fetch_body_is_native_bytes_and_can_be_decoded() {
    let module = Module::compile("async def run(ctx):\n    response = await ctx.fetch('http://test/')\n    return response.json()").unwrap();
    let (mut session, event) =
        Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    assert_eq!(event["operation"], "fetch");
    let result = session
        .resume_fetch(200, json!({}), b"{\"ok\":true}".to_vec())
        .unwrap();
    assert_eq!(result["done"], true);
    assert_eq!(body(&mut session), "{\"ok\":true}");
}

fn body(session: &mut Session) -> String {
    String::from_utf8(session.take_response().unwrap().body).unwrap()
}

#[test]
fn fetched_binary_responses_stay_native_through_completion() {
    let module =
        Module::compile("async def run(ctx): return await ctx.fetch('http://test/')").unwrap();
    let (mut session, _) =
        Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    let bytes = vec![255; 400000];
    let event = session
        .resume_fetch(201, json!({"x-test":"native"}), bytes.clone())
        .unwrap();
    assert_eq!(event, json!({"done":true}));
    let response = session.take_response().unwrap();
    assert_eq!(response.status, 201);
    assert_eq!(response.body, bytes);
    assert_eq!(response.headers, vec![("x-test".into(), "native".into())]);
    assert!(session.take_response().is_none());
}

#[test]
fn outbound_binary_bodies_do_not_expand_into_python_or_json_lists() {
    let module = Module::compile("async def run(ctx): return await ctx.fetch('http://test/', method='POST', body=b'\\xff' * 400000)").unwrap();
    let (mut session, event) =
        Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    assert_eq!(event["args"], json!(["http://test/", "POST", {}, null]));
    assert_eq!(session.take_fetch_body().unwrap(), vec![255; 400000]);
    assert!(session.take_fetch_body().is_none());
    assert_eq!(
        session.resume_fetch(204, json!({}), vec![]).unwrap()["done"],
        true
    );
    assert_eq!(session.take_response().unwrap().status, 204);
}

#[test]
fn filesystem_suspends_with_native_bytes_and_catchable_errors() {
    use celld_runtime::filesystem::*;
    let module = Module::compile("from pathlib import Path\ndef run():\n    p = Path('notes.txt')\n    n = p.write_text('hé')\n    try:\n        p.read_bytes()\n    except FileNotFoundError:\n        return n\n").unwrap();
    let (mut session, event) = Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    assert_eq!(event["operation"], "filesystem");
    match session.take_filesystem_call().unwrap() {
        FsCall::Write { path, data, append } => {
            assert_eq!(path, "notes.txt"); assert_eq!(data, "hé".as_bytes()); assert!(!append);
        }
        _ => panic!("expected write"),
    }
    session.resume_filesystem(Ok(FsReply::None)).unwrap();
    assert!(matches!(session.take_filesystem_call().unwrap(), FsCall::Read(_)));
    let event = session.resume_filesystem(Err(FsError::new(FsErrorKind::NotFound, "gone"))).unwrap();
    assert_eq!(event["done"], true);
    assert_eq!(body(&mut session), "2");
}

#[test]
fn filesystem_open_preserves_binary_buffers_and_append() {
    use celld_runtime::filesystem::*;
    let module = Module::compile("def run():\n    with open('data', 'ab') as f:\n        return f.write(b'\\x00\\xff')\n").unwrap();
    let (mut session, _) = Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    assert!(matches!(session.take_filesystem_call().unwrap(), FsCall::Open { create: true, truncate: false, .. }));
    session.resume_filesystem(Ok(FsReply::Path("/data".into()))).unwrap();
    match session.take_filesystem_call().unwrap() {
        FsCall::Write { data, append, .. } => { assert_eq!(data, [0,255]); assert!(append); }
        _ => panic!("expected append"),
    }
    session.resume_filesystem(Ok(FsReply::None)).unwrap();
    assert_eq!(body(&mut session), "2");
}

#[test]
fn filesystem_text_decode_errors_keep_python_details() {
    use celld_runtime::filesystem::*;
    let module = Module::compile("from pathlib import Path\ndef run():\n    try:\n        Path('binary').read_text()\n    except UnicodeDecodeError as e:\n        return e.start\n").unwrap();
    let (mut session, _) = Session::start(module.get("run").unwrap(), &json!({}), &json!({})).unwrap();
    session.take_filesystem_call().unwrap();
    session.resume_filesystem(Ok(FsReply::Bytes(vec![b'a', 255]))).unwrap();
    assert_eq!(body(&mut session), "1");
}
