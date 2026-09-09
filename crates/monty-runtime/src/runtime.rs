//! The celld integration uses only the public native runtime contract.
use crate::{
    Failure, Session,
    exports::{Module, prepare},
    value,
};
use api::{HostCall, HostReply, Response, Step};
use celld_runtime as api;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct Monty {
    expose_errors: bool,
    pub(crate) network: crate::network::Network,
    pub(crate) extensions: Arc<crate::extensions::Extensions>,
}
impl Monty {
    /// Include exception messages in HTTP responses (off by default).
    pub fn with_public_errors(mut self, enabled: bool) -> Self {
        self.expose_errors = enabled;
        self
    }
    pub fn new() -> Self {
        Self::default()
    }
}
const DESCRIPTOR: api::Descriptor = api::Descriptor {
    extension: "py",
    main_module: "index.py",
    artifact_prefix: "# celld:monty-native-v1\n",
    required_feature: "monty-http-v1",
};
impl api::Runtime for Monty {
    fn descriptor(&self) -> &api::Descriptor {
        &DESCRIPTOR
    }
    fn supported_features(&self) -> Vec<&'static str> {
        vec![
            "monty-native-v1",
            "monty-modules-v1",
            "monty-network-policy-v1",
            "monty-filesystem-v1",
            "monty-execution-v1",
            "monty-observability-v1",
            "monty-http-v1",
        ]
    }
    fn types(&self) -> &str {
        &self.extensions.modules["celld"].types
    }
    fn type_files(&self) -> std::collections::BTreeMap<String, String> {
        let mut files = std::collections::BTreeMap::new();
        for (name, module) in &self.extensions.modules {
            let mut parts = name.split('.').collect::<Vec<_>>();
            let leaf = parts.pop().unwrap();
            let mut directory = String::new();
            for part in parts {
                directory.push_str(part);
                directory.push('/');
                files
                    .entry(format!("{directory}__init__.pyi"))
                    .or_insert_with(String::new);
            }
            let has_children = self
                .extensions
                .modules
                .keys()
                .any(|n| n.starts_with(&format!("{name}.")));
            let path = if has_children {
                format!("{directory}{leaf}/__init__.pyi")
            } else {
                format!("{directory}{leaf}.pyi")
            };
            files.insert(path, module.types.clone());
        }
        files
    }
    fn is_entry(&self, path: &std::path::Path) -> bool {
        path.extension().is_some_and(|e| e == "py")
            || (path.is_dir() && path.join("__init__.py").is_file())
    }
    fn bundle(&self, root: &std::path::Path, entry: &std::path::Path) -> api::Result<String> {
        Ok(crate::package::Package::read(root, entry, &self.extensions.sources())?.encode()?)
    }
    fn compile(&self, source: &str) -> api::Result<Box<dyn api::Program>> {
        let compiled = prepare(source, &self.extensions)?;
        Ok(Box::new(Program {
            expose_errors: self.expose_errors,
            network: self.network.clone(),
            http: Module::compile_graph(
                &compiled.graph,
                &compiled.entry,
                None,
                self.extensions.clone(),
            )?,
            objects: compiled
                .classes
                .iter()
                .map(|(identity, module, class)| {
                    Module::compile_graph(
                        &compiled.graph,
                        &compiled.entry,
                        Some((module, class)),
                        self.extensions.clone(),
                    )
                    .map(|m| (identity.clone(), m))
                })
                .collect::<Result<_, _>>()?,
            classes: compiled.classes.into_iter().map(|(id, _, _)| id).collect(),
        }))
    }
}

#[derive(Clone)]
struct Program {
    expose_errors: bool,
    network: crate::network::Network,
    http: Module,
    objects: HashMap<String, Module>,
    classes: Vec<String>,
}
impl api::Program for Program {
    fn fork(&self) -> Box<dyn api::Program> {
        Box::new(self.clone())
    }
    fn classes(&self) -> &[String] {
        &self.classes
    }
    fn error_response(&self, error: api::Failure, durable: bool) -> Response {
        let message = if self.expose_errors || error.status < 500 || durable {
            error.message
        } else {
            "Python execution failed".into()
        };
        let error = json!({"status":error.status,"code":error.code,"message":message});
        json_response(
            if durable {
                200
            } else {
                error["status"].as_u64().unwrap() as u16
            },
            json!({"error":error}),
        )
    }
    fn start(&self, call: api::Invocation) -> api::Result<(Box<dyn api::Execution>, Step)> {
        self.start_inner(call).map_err(Into::into)
    }
}
impl Program {
    fn start_inner(
        &self,
        call: api::Invocation,
    ) -> Result<(Box<dyn api::Execution>, Step), Failure> {
        call.limits.validate()?;
        if call.request.payload_bytes() > call.limits.max_payload_bytes {
            return Err("request exceeds payload limit".into());
        }
        let fetch_context = crate::FetchContext {
            execution: call.execution.clone(),
            limits: call.limits,
            request_url: call.request.url.clone(),
            env: call.env.clone(),
            object: call
                .object
                .as_ref()
                .map(|o| (o.class.clone(), o.id.clone())),
            alarm: call.alarm,
        };
        let input = call.request;
        let url =
            url::Url::parse(&input.url).map_err(|_| Failure::arguments("invalid request URL"))?;
        let path = url.path().strip_prefix('/').unwrap_or("");
        let name = match percent_encoding::percent_decode_str(path).decode_utf8() {
            Ok(name) => name,
            Err(_) if call.object.is_none() && self.http.http().is_some() => std::borrow::Cow::Borrowed(""),
            Err(_) => return Err(Failure::arguments("invalid handler name")),
        };
        // An exact public function path is reserved, including its POST-only contract.
        // All other stateless requests go to the explicitly exported HTTP handler.
        let function_route =
            !path.contains('/') && !name.starts_with('_') && self.http.get(&name).is_some();
        let http = if call.object.is_none() && !function_route {
            self.http.http()
        } else {
            None
        };
        if !call.alarm && http.is_none() {
            if path.contains('/')
                || name.starts_with('_')
                || (call.object.is_none() && !function_route)
            {
                return complete(text_response(404, "unknown handler", vec![]));
            }
            if input.method != "POST" {
                return complete(text_response(
                    405,
                    "POST required",
                    vec![("allow".into(), "POST".into())],
                ));
            }
        }
        let args: Value = if http.is_some() || input.body.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&input.body)
                .map_err(|_| Failure::arguments("request body must be a UTF-8 JSON object"))?
        };
        let request_body = if call.object.is_none() { input.body.as_slice() } else { &[] };
        let (function, args, mut metadata) = if let Some(object) = call.object {
            if !call.alarm && name == "alarm" {
                return Err("alarm is dispatched by the host".into());
            }
            let function = self
                .objects
                .get(&object.class)
                .and_then(|m| m.get(if call.alarm { "alarm" } else { &name }))
                .ok_or(if call.alarm {
                    "define alarm(self) before scheduling an alarm"
                } else {
                    "unknown public method"
                })?;
            let arguments =
                if call.alarm {
                    json!({})
                } else {
                    let wire = args["wire"]
                        .as_str()
                        .ok_or_else(|| Failure::arguments("missing typed object arguments"))?;
                    value::to_json(&serde_json::from_str(wire).map_err(|e| {
                        Failure::arguments(format!("invalid object arguments: {e}"))
                    })?)?
                };
            (
                function,
                arguments,
                json!({"id":object.id,"env":call.env,"request":if call.alarm { json!({}) } else { args["request"].clone() }}),
            )
        } else {
            let function = http
                .or_else(|| self.http.get(&name))
                .ok_or("unknown handler")?;
            (
                function,
                args,
                json!({"id":null,"env":call.env,"request":{"url":input.url,"method":input.method,"headers":headers_object(input.headers.clone()),"header_items":input.headers,"path":url.path(),"query_items":url.query_pairs().collect::<Vec<_>>()}}),
            )
        };
        metadata["execution"] = serde_json::to_value(&call.execution).map_err(|e| e.to_string())?;
        metadata["limits"] = serde_json::to_value(call.limits).map_err(|e| e.to_string())?;
        let (session, event) =
            Session::start_with_limits(function, &args, &metadata, request_body, call.limits, call.observer)?;
        let mut execution = Execution {
            session: Some(session),
            network: self.network.clone(),
            fetch_context,
            request: metadata["request"].clone(),
            spans: HashMap::new(),
            next_span: 0,
        };
        let step = execution.event(event)?;
        Ok((Box::new(execution), step))
    }
}

enum Event {
    Step(Step),
    Resume(Response),
    ResumeValue(Value),
}

struct Execution {
    network: crate::network::Network,
    fetch_context: crate::FetchContext,
    session: Option<Session>,
    request: Value,
    spans: HashMap<u64, (String, Value, i64, std::time::Instant)>,
    next_span: u64,
}
impl api::Execution for Execution {
    fn resume(&mut self, reply: HostReply) -> api::Result<Step> {
        self.resume_inner(reply).map_err(Into::into)
    }
}
impl Execution {
    fn resume_inner(&mut self, reply: HostReply) -> Result<Step, Failure> {
        if reply.payload_bytes() > self.fetch_context.limits.max_payload_bytes {
            return Err("host reply exceeds payload limit".into());
        }
        let session = self.session.as_mut().ok_or("execution already completed")?;
        let event = match reply {
            HostReply::Filesystem(reply) => session.resume_filesystem(reply)?,
            HostReply::Fetch(r) => {
                session.resume_fetch(r.status, headers_object(r.headers), r.body)?
            }
            HostReply::Object(r) => session.resume(
                serde_json::from_slice(&r.body)
                    .map_err(|e| format!("invalid durable response: {e}"))?,
            )?,
            HostReply::Value(v) => session.resume(json!({"result":v}))?,
            HostReply::Timestamp(Some(ms)) => session.resume(value::timestamp_reply(ms)?)?,
            HostReply::Timestamp(None) => session.resume(json!({"result":null}))?,
            HostReply::Error(error) => session.resume(json!({"error":error}))?,
        };
        self.event(event)
    }
    fn event(&mut self, mut event: Value) -> Result<Step, Failure> {
        loop {
            let suspended = event["done"] != true;
            match self.event_once(event) {
                Err(error) if suspended => {
                    // Host argument errors are catchable Python exceptions,
                    // including validation performed at this typed boundary.
                    event = self
                        .session
                        .as_mut()
                        .ok_or("execution already completed")?
                        .resume(json!({"error":error.message}))?;
                }
                Ok(Event::ResumeValue(value)) => {
                    event = self
                        .session
                        .as_mut()
                        .ok_or("execution already completed")?
                        .resume(json!({"result":value}))?;
                }
                Ok(Event::Resume(response)) => {
                    // Interpreter failures after a synthetic reply are final,
                    // not argument errors on the already-consumed host call.
                    event = self
                        .session
                        .as_mut()
                        .ok_or("execution already completed")?
                        .resume_fetch(
                            response.status,
                            headers_object(response.headers),
                            response.body,
                        )?;
                }
                Ok(Event::Step(step)) => return Ok(step),
                Err(error) => return Err(error),
            }
        }
    }
    fn event_once(&mut self, event: Value) -> Result<Event, Failure> {
        let session = self.session.as_mut().ok_or("execution already completed")?;
        if event["done"] == true {
            let response = match session.take_response() {
                Some(r) => Response {
                    status: r.status,
                    headers: r.headers,
                    body: r.body,
                },
                None => json_response(200, json!({"wire":event["wire"]})),
            };
            if response.payload_bytes() > self.fetch_context.limits.max_payload_bytes {
                return Err("response exceeds payload limit".into());
            }
            self.session.take();
            return Ok(Event::Step(Step::Return(response)));
        }
        let args = &event["args"];
        let text = |i: usize| {
            args[i]
                .as_str()
                .map(str::to_owned)
                .ok_or("expected string argument".to_owned())
        };
        let call = match event["operation"]
            .as_str()
            .ok_or("missing host operation")?
        {
            "filesystem" => HostCall::Filesystem(session.take_filesystem_call()?),
            "storage.get" => HostCall::Get(text(0)?),
            "storage.put" => HostCall::Put(text(0)?, args[1].clone()),
            "storage.delete" => HostCall::Delete(text(0)?),
            "storage.delete_all" => HostCall::Clear,
            "storage.list" => HostCall::List {
                prefix: text(0)?,
                limit: args[1]
                    .as_u64()
                    .filter(|n| *n <= 1000)
                    .ok_or("list limit must be 0-1000")? as usize,
                reverse: args[2].as_bool().ok_or("reverse must be bool")?,
            },
            "storage.sql" => HostCall::Sql {
                query: text(0)?,
                bindings: args[1]
                    .as_array()
                    .ok_or("SQL bindings must be an array")?
                    .clone(),
            },
            "storage.get_alarm" => HostCall::GetAlarm,
            "storage.set_alarm" => {
                let at = if let Some(seconds) = args[0].as_f64() {
                    let at = chrono::Utc::now().timestamp_millis() as f64 + seconds * 1000.0;
                    if !at.is_finite() || at < 0.0 || at >= i64::MAX as f64 {
                        return Err("invalid alarm duration".into());
                    }
                    at as i64
                } else {
                    chrono::DateTime::parse_from_rfc3339(&text(0)?)
                        .map_err(|_| "alarm datetime must include a timezone")?
                        .timestamp_millis()
                };
                HostCall::SetAlarm(at)
            }
            "storage.delete_alarm" => HostCall::DeleteAlarm,
            "storage.transaction_begin" => HostCall::BeginTransaction,
            "storage.transaction_commit" => HostCall::CommitTransaction,
            "storage.transaction_rollback" => HostCall::RollbackTransaction,
            "storage.sync" => HostCall::Sync,
            "sleep" => HostCall::Sleep(std::time::Duration::from_secs_f64(
                args[0]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 30.0)
                    .ok_or("sleep must be between 0 and 30 seconds")?,
            )),
            "fetch" => HostCall::Fetch(api::Request {
                url: text(0)?,
                method: text(1)?,
                headers: args[2]
                    .as_object()
                    .ok_or("invalid fetch headers")?
                    .iter()
                    .map(|(k, v)| {
                        v.as_str()
                            .map(|v| (k.clone(), v.to_owned()))
                            .ok_or("invalid header")
                    })
                    .collect::<Result<_, _>>()?,
                body: match session.take_fetch_body() {
                    Some(body) => body,
                    None => match &args[3] {
                        Value::Null => Vec::new(),
                        Value::String(body) => body.as_bytes().to_vec(),
                        _ => return Err("invalid fetch body".into()),
                    },
                },
            }),
            "object.call" => HostCall::CallObject {
                object: api::Object {
                    class: text(0)?,
                    id: text(1)?,
                },
                method: text(2)?,
                body: serde_json::to_vec(&json!({"wire":args[3],"request":self.request}))
                    .map_err(|e| e.to_string())?,
            },
            "now" => HostCall::Now,
            "uuid" => HostCall::Uuid,
            "log" => {
                let level = args[1].as_str().unwrap_or("info");
                if !["debug", "info", "warn", "error"].contains(&level) {
                    return Err("invalid log level".into());
                }
                let fields = args.get(2).cloned().unwrap_or_else(|| json!({}));
                if !fields.is_object() {
                    return Err("log fields must be an object".into());
                }
                session.output().flush();
                session.output().emit(api::observability::Diagnostic::Log {
                    level: level.into(),
                    message: crate::observability::cap(&text(0)?, 6144),
                    fields,
                });
                return Ok(Event::ResumeValue(Value::Null));
            }
            "span" => {
                if args[0] == "start" {
                    if self.spans.len() >= 16 || self.next_span >= 128 {
                        return Err("custom span limit exceeded".into());
                    }
                    let name = text(1)?;
                    if name.is_empty()
                        || name.len() > 128
                        || !args[2].is_object()
                        || args[2].to_string().len() > 4096
                    {
                        return Err("invalid span name or fields".into());
                    }
                    self.next_span += 1;
                    self.spans.insert(
                        self.next_span,
                        (
                            name,
                            args[2].clone(),
                            chrono::Utc::now().timestamp_micros(),
                            std::time::Instant::now(),
                        ),
                    );
                    return Ok(Event::ResumeValue(json!(self.next_span)));
                }
                let id = args[1].as_u64().ok_or("invalid span token")?;
                let (name, fields, start_unix_us, started) =
                    self.spans.remove(&id).ok_or("unknown or completed span")?;
                session.output().emit(api::observability::Diagnostic::Span {
                    name,
                    fields,
                    start_unix_us,
                    duration_us: started.elapsed().as_micros().min(i64::MAX as u128) as i64,
                    ok: args[2].as_bool().ok_or("invalid span status")?,
                });
                return Ok(Event::ResumeValue(Value::Null));
            }
            _ => return Err("unknown capability".into()),
        };
        if call.payload_bytes() > self.fetch_context.limits.max_payload_bytes {
            return Err("host call exceeds payload limit".into());
        }
        if let HostCall::Fetch(request) = call {
            return match self.network.intercept(&self.fetch_context, request) {
                crate::FetchDecision::Forward(request) => {
                    Ok(Event::Step(Step::Call(HostCall::Fetch(api::Request {
                        url: request.url.into(),
                        method: request.method,
                        headers: request.headers,
                        body: request.body,
                    }))))
                }
                crate::FetchDecision::Respond(response) => {
                    if response.payload_bytes() > self.fetch_context.limits.max_payload_bytes {
                        return Err("fetch response exceeds 1 MiB".into());
                    }
                    // The outer event loop resumes locally, avoiding recursive
                    // Rust calls when Python repeatedly fetches synthetic responses.
                    Ok(Event::Resume(response))
                }
                crate::FetchDecision::Deny(error) => Err(error.into()),
            };
        }
        Ok(Event::Step(Step::Call(call)))
    }
}

impl From<Failure> for api::Failure {
    fn from(e: Failure) -> Self {
        Self {
            status: e.status,
            code: e.code,
            message: e.message,
        }
    }
}
fn complete(response: Response) -> Result<(Box<dyn api::Execution>, Step), Failure> {
    Ok((
        Box::new(Execution {
            session: None,
            network: crate::network::Network::default(),
            fetch_context: crate::FetchContext {
                execution: Default::default(),
                limits: Default::default(),
                request_url: String::new(),
                env: Value::Null,
                object: None,
                alarm: false,
            },
            request: Value::Null,
            spans: HashMap::new(),
            next_span: 0,
        }),
        Step::Return(response),
    ))
}
fn json_response(status: u16, value: Value) -> Response {
    Response {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: value.to_string().into_bytes(),
    }
}
fn text_response(status: u16, body: &str, mut headers: Vec<(String, String)>) -> Response {
    headers.push(("content-type".into(), "text/plain;charset=UTF-8".into()));
    Response {
        status,
        headers,
        body: body.as_bytes().to_vec(),
    }
}
fn headers_object(headers: Vec<(String, String)>) -> Value {
    let mut result = serde_json::Map::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        match result.get_mut(&name) {
            Some(Value::String(previous)) if name != "set-cookie" => {
                previous.push_str(", ");
                previous.push_str(&value);
            }
            _ => {
                result.insert(name, json!(value));
            }
        }
    }
    Value::Object(result)
}
