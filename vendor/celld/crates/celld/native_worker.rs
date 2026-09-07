//! Native worker host. No V8 isolate, JavaScript bootstrap, promise,
//! or value conversion is involved in a native invocation. The shared driver
//! owns asynchronous futures; this backend only runs bounded interpreter turns.
use super::*;
use crate::native_host::Host;
use celld_runtime::{self as api, Failure, HostCall, HostReply, Step};
use serde_json::{Value, json};

const BODY_LIMIT: usize = 1024 * 1024;

#[cfg(test)]
#[path = "../../../../tests/native_durability.rs"]
mod tests;

pub struct Worker {
    config: Arc<WorkerConfig>,
    program: Box<dyn api::Program>,
    env: Value,
    cells: storage::Cells,
    live: Arc<()>,
}

struct InputGuard {
    scope: String,
    event: u64,
}
impl InputGuard {
    fn acquire(scope: &str) -> Self {
        let event = NEXT_GATE_EVENT.fetch_add(1, Ordering::Relaxed);
        assert!(
            cell_gates()
                .lock()
                .unwrap()
                .entry(scope.to_owned())
                .or_default()
                .take(event, None, &mut None),
            "native turn delivered behind a closed gate"
        );
        Self {
            scope: scope.to_owned(),
            event,
        }
    }
}
impl Drop for InputGuard {
    fn drop(&mut self) {
        release_cell_gate(&self.scope, self.event, Ok(()));
    }
}

struct Incoming {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
}

pub(super) struct Execution {
    session: Option<Box<dyn api::Execution>>,
    host: Host,
    input: Option<Incoming>,
    _gate: Option<InputGuard>,
    _live: Arc<()>,
}

impl Worker {
    pub fn load_config(config: Arc<WorkerConfig>) -> Result<Self> {
        let program = config
            .native_program
            .get_or_init(|| {
                let runtime = crate::native::runtime().expect("registered native runtime");
                runtime
                    .compile(
                        config
                            .src
                            .strip_prefix(runtime.descriptor().artifact_prefix)
                            .expect("native artifact"),
                    )
                    .map(Mutex::new)
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| anyhow!(e.clone()))?
            .lock()
            .unwrap()
            .fork();
        for class in &config.do_classes {
            anyhow::ensure!(
                program.classes().iter().any(|name| name == class),
                "unknown Native runtime durable class: {class}"
            );
        }
        let mut env: serde_json::Map<String, Value> = config
            .vars
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        if let Some(extra) = &config.loader_env {
            for (key, value) in serde_json::from_str::<serde_json::Map<String, Value>>(extra)? {
                if value.is_string() {
                    env.insert(key, value);
                } else {
                    env.remove(&key);
                }
            }
        }
        Ok(Self {
            program,
            config,
            env: Value::Object(env),
            cells: Default::default(),
            live: Arc::new(()),
        })
    }

    pub fn own_cell(
        &mut self,
        scope: &str,
        authority: Option<CellStorage<'_>>,
    ) -> Result<Option<i64>> {
        let _cells = self.cells.install();
        match authority {
            Some(authority) => {
                let class = scope
                    .split_once(':')
                    .map(|(class, _)| class)
                    .ok_or_else(|| anyhow!("invalid durable scope"))?;
                anyhow::ensure!(
                    self.program.classes().iter().any(|name| name == class),
                    "unknown Native runtime durable class"
                );
                storage::open_at_epoch(
                    scope,
                    authority.path,
                    authority.epoch,
                    authority.vfs,
                    self.config.compat.sqlite_vec,
                )?;
                if let Some(name) = storage::get_actor_name(scope)? {
                    self.validate_name(scope, &name)?;
                }
                Ok(storage::get_alarm(scope))
            }
            None => {
                storage::close(scope);
                Ok(None)
            }
        }
    }
    fn validate_name(&self, scope: &str, name: &str) -> Result<()> {
        let (class, id) = scope
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid durable scope"))?;
        let key = namespace_key(&self.config.script_name, class);
        let expected = durable_object_id_hex(&durable_object_id_for_name(&key, name));
        anyhow::ensure!(
            id == expected,
            "actor name does not match Durable Object ID for {scope}"
        );
        Ok(())
    }
    pub fn set_id_name(&mut self, scope: &str, name: &str) -> Result<()> {
        let _cells = self.cells.install();
        self.validate_name(scope, name)?;
        storage::set_actor_name(scope, name)?;
        Ok(())
    }
    pub fn take_alarm_moves(&mut self) -> Vec<(String, i64)> {
        let _cells = self.cells.install();
        storage::take_alarm_moves()
    }

    fn entry(
        &self,
        reply: Answer,
        scope: Option<String>,
        request_id: Option<RequestId>,
        trace: Option<crate::telemetry::TraceContext>,
    ) -> InFlight {
        let context = IoContext::new();
        let writes_before = scope.as_deref().and_then(storage::write_position);
        context.begin_event();
        if let Some(scope) = &scope {
            context.egress.lock().unwrap().push(EgressFrame {
                storage: scope.clone(),
                before: writes_before.unwrap_or(0),
                root: None,
            });
        }
        let gate = scope.as_deref().map(InputGuard::acquire);
        InFlight {
            promise: None,
            runtime_state: None,
            context,
            writes_before,
            request_id,
            active_request_id: None,
            reply: Some(reply),
            gated_reply: None,
            background: None,
            ops: Default::default(),
            io_context_ops: Default::default(),
            alarm: None,
            started: Instant::now(),
            trace,
            failure: None,
            native: Some(Execution {
                session: None,
                host: Host::new(scope.clone()),
                input: None,
                _gate: gate,
                _live: self.live.clone(),
            }),
            scope,
        }
    }

    pub fn turn_begin(
        &mut self,
        job: crate::WorkerJob,
        trace: Option<crate::telemetry::TraceContext>,
    ) -> (Option<InFlight>, Vec<Op>) {
        let _cells = self.cells.install();
        let (url, method, body, headers, request_id, reply) = match job {
            crate::WorkerJob::Fetch {
                url,
                method,
                body,
                headers,
                request_id,
                reply,
                ..
            } => (url, method, body, headers, request_id, reply),
            crate::WorkerJob::Rpc { reply, .. } => {
                let _ = reply.send(Err(anyhow!(
                    "Native runtime functions are exposed as POST handlers"
                )));
                return (None, vec![]);
            }
            crate::WorkerJob::Queue { reply, .. } => {
                let _ = reply.send(Err(anyhow!(
                    "Native runtime queue consumers are not supported"
                )));
                return (None, vec![]);
            }
        };
        self.begin_fetch(url, method, body, headers, request_id, reply, None, trace)
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_fetch(
        &self,
        url: String,
        method: String,
        body: RequestBody,
        headers: Vec<(String, String)>,
        request_id: Option<RequestId>,
        reply: tokio::sync::oneshot::Sender<Result<HttpResponse>>,
        scope: Option<String>,
        trace: Option<crate::telemetry::TraceContext>,
    ) -> (Option<InFlight>, Vec<Op>) {
        if Arc::strong_count(&self.live) > 256 {
            let _ = reply.send(Ok(response(
                self.program.error_response(
                    Failure {
                        status: 503,
                        code: "overloaded".into(),
                        message: "native invocation capacity exceeded".into(),
                    },
                    false,
                ),
                (None, None),
            )));
            return (None, vec![]);
        }
        let mut entry = self.entry(Answer::Fetch(reply), scope, request_id, trace);
        let _context = CurrentGuard::enter(entry.context.clone());
        if take_request_cancellation(request_id) {
            entry.fail_cancelled(anyhow!("The client has disconnected"));
            return (Some(entry), vec![]);
        }
        let input = Incoming {
            url,
            method,
            headers,
        };
        let result = match body {
            RequestBody::Bytes(body) => self.http(&mut entry, input, &body),
            RequestBody::Stream(id) => {
                entry.context.own_body_stream(id);
                entry.native.as_mut().unwrap().input = Some(input);
                match take_body_stream(id) {
                    Ok(stream) => {
                        asyncrt::enqueue(async move { collect(stream).await });
                        Ok(None)
                    }
                    Err(error) => Err(error.into()),
                }
            }
        };
        self.process(&mut entry, result);
        let ops = adopt(&mut entry);
        (Some(entry), ops)
    }

    fn http(
        &self,
        entry: &mut InFlight,
        input: Incoming,
        body: &[u8],
    ) -> Result<Option<Step>, Failure> {
        if body.len() > BODY_LIMIT {
            return Err(body_limit());
        }
        let object = self.object(entry.scope.as_deref())?;
        let (session, event) = self.program.start(api::Invocation {
            request: api::Request {
                url: input.url,
                method: input.method,
                headers: input.headers,
                body: body.to_vec(),
            },
            object,
            alarm: false,
            env: self.env.clone(),
        })?;
        entry.native.as_mut().unwrap().session = Some(session);
        Ok(Some(event))
    }
    fn object(&self, scope: Option<&str>) -> Result<Option<api::Object>, Failure> {
        scope
            .map(|scope| {
                let class = scope
                    .split_once(':')
                    .ok_or("invalid durable scope")?
                    .0
                    .to_owned();
                let id = storage::get_actor_name(scope)
                    .map_err(|e| e.to_string())?
                    .ok_or("native durable objects require a named ID")?;
                Ok(api::Object { class, id })
            })
            .transpose()
    }

    pub fn turn_begin_cell(
        &mut self,
        job: CellJob,
        trace: Option<crate::telemetry::TraceContext>,
    ) -> (Option<InFlight>, Vec<Op>) {
        let _cells = self.cells.install();
        if Arc::strong_count(&self.live) > 256 {
            job.fail(anyhow!("Native runtime invocation capacity exceeded"));
            return (None, vec![]);
        }
        let (scope, answer, alarm) = match job {
            CellJob::Fetch {
                scope,
                name,
                url,
                method,
                body,
                headers,
                request_id,
                reply,
                ..
            } => {
                if let Some(name) = name {
                    if let Err(error) = self.set_id_name(&scope, &name) {
                        let _ = reply.send(Err(error));
                        return (None, vec![]);
                    }
                }
                return self.begin_fetch(
                    url,
                    method,
                    body,
                    headers,
                    request_id,
                    reply,
                    Some(scope),
                    trace,
                );
            }
            CellJob::Alarm {
                scope,
                scheduled_ms,
                claim: _,
                reply,
                request_id,
            } => {
                let now = unix_now_ms();
                if now < scheduled_ms {
                    let _ = reply.send(Err(anyhow!("alarm dispatched before its deadline")));
                    return (None, vec![]);
                }
                let Some((scheduled, _retry)) = storage::due_alarm_entry(&scope, now) else {
                    let _ = reply.send(Ok((storage::get_alarm(&scope), None)));
                    return (None, vec![]);
                };
                storage::begin_alarm_handler(&scope, scheduled);
                (
                    scope,
                    Answer::Alarm(reply),
                    Some((AlarmClaim { now_ms: now }, request_id)),
                )
            }
            job => {
                job.fail(anyhow!(
                    "Native runtime durable objects expose methods and alarms"
                ));
                return (None, vec![]);
            }
        };
        let mut entry = self.entry(
            answer,
            Some(scope.clone()),
            alarm.as_ref().and_then(|(_, id)| *id),
            trace,
        );
        entry.alarm = alarm.map(|(claim, _)| claim);
        let _context = CurrentGuard::enter(entry.context.clone());
        let result = (|| {
            let (session, event) = self.program.start(api::Invocation {
                request: api::Request {
                    url: "http://native.internal/alarm".into(),
                    method: "POST".into(),
                    headers: vec![],
                    body: vec![],
                },
                object: self.object(Some(&scope))?,
                alarm: true,
                env: self.env.clone(),
            })?;
            entry.native.as_mut().unwrap().session = Some(session);
            Ok(Some(event))
        })();
        self.process(&mut entry, result);
        let ops = adopt(&mut entry);
        (Some(entry), ops)
    }

    pub fn turn_deliver(
        &mut self,
        entry: &mut InFlight,
        op: u64,
        result: Result<asyncrt::OpOut, String>,
    ) -> Vec<Op> {
        let _cells = self.cells.install();
        let _context = CurrentGuard::enter(entry.context.clone());
        entry.ops.remove(&op);
        if take_request_cancellation(entry.request_id) {
            self.turn_cancel(entry);
            return vec![];
        }
        let Some(execution) = entry.native.as_mut() else {
            return vec![];
        };
        let result = if let Some(input) = execution.input.take() {
            match result {
                Ok(asyncrt::OpOut::Bytes(body)) => self.http(entry, input, &body),
                Err(error) if error == "body exceeds 1 MiB" => Err(body_limit()),
                Err(error) => Err(error.into()),
                _ => Err("invalid native body result".into()),
            }
        } else {
            let session = execution.session.as_mut().expect("suspended session");
            match result {
                Ok(asyncrt::OpOut::Native(reply)) => session.resume(reply),
                Err(error) => session.resume(HostReply::Error(error)),
                _ => Err("invalid native operation result".into()),
            }
            .map(Some)
        };
        self.process(entry, result);
        adopt(entry)
    }

    fn process(&self, entry: &mut InFlight, result: Result<Option<Step>, Failure>) {
        let result = result.and_then(|event| match event {
            Some(event) => self.drive(entry, event),
            None => Ok(None),
        });
        match result {
            Ok(Some(event)) => self.finish(entry, Ok(event)),
            Err(error) => self.finish(entry, Err(error)),
            Ok(None) => {}
        }
    }
    fn drive(&self, entry: &mut InFlight, mut event: Step) -> Result<Option<Step>, Failure> {
        loop {
            let execution = entry.native.as_mut().expect("live invocation");
            let Step::Call(call) = event else {
                if execution.host.in_transaction() {
                    return Err("unclosed transaction".into());
                }
                return Ok(Some(event));
            };
            let reply = if Host::handles(&call) {
                match execution.host.call(call) {
                    Ok((reply, alarm)) => {
                        if let (Some(scope), Some(at)) = (&execution.host.scope, alarm) {
                            if at >= 0 {
                                spawn_arm_gate(scope, at, Some(entry.context.clone()));
                            }
                        }
                        reply
                    }
                    Err(error) => HostReply::Error(error),
                }
            } else if execution.host.in_transaction() {
                HostReply::Error("external I/O is not allowed inside a storage transaction".into())
            } else {
                match self.suspend(entry, call) {
                    Ok(()) => return Ok(None),
                    Err(error) => HostReply::Error(error),
                }
            };
            event = entry
                .native
                .as_mut()
                .unwrap()
                .session
                .as_mut()
                .expect("running session")
                .resume(reply)?;
        }
    }

    fn suspend(&self, entry: &mut InFlight, call: HostCall) -> Result<(), String> {
        match call {
            HostCall::Sync => {
                let scope = entry
                    .scope
                    .as_deref()
                    .ok_or("storage requires a durable object")?;
                if GATE_TX.get().is_none() {
                    return Err("storage.sync: this process has no output-gate channel".into());
                }
                let sample =
                    storage::sync_sample(scope).ok_or("storage.sync: cell is not resident")?;
                if sample.in_transaction {
                    return Err("storage.sync: transaction must commit before syncing".into());
                }
                // Sync always proves committed state, including writes from
                // earlier events. A read-only response ticket cannot do that.
                let gate = EgressGate::Wrote(
                    scope.to_owned(),
                    celld_logic::Channel::Sync,
                    sample.position,
                    Some(sample.epoch),
                );
                asyncrt::enqueue(async move {
                    await_egress_gate(gate).await?;
                    Ok(asyncrt::OpOut::Native(HostReply::Value(Value::Null)))
                });
            }
            HostCall::Sleep(duration) => {
                if duration > Duration::from_secs(30) {
                    return Err("sleep exceeds 30 seconds".into());
                }
                asyncrt::enqueue(async move {
                    asyncrt::sleep(duration).await;
                    Ok(asyncrt::OpOut::Native(HostReply::Value(Value::Null)))
                });
            }
            HostCall::Fetch(request) => {
                if self.config.egress == EgressPolicy::Deny {
                    return Err(
                        "This worker is not permitted to access the internet via fetch".into(),
                    );
                }
                let api::Request {
                    url,
                    method,
                    headers,
                    body,
                } = request;
                if body.len() > BODY_LIMIT {
                    return Err("fetch body exceeds 1 MiB".into());
                }
                let future = native_fetch(
                    api::Request {
                        url,
                        method,
                        headers,
                        body,
                    },
                    entry.context.clone(),
                    entry.trace,
                );
                asyncrt::enqueue(async move {
                    let response = future.await?;
                    let status = response.status().as_u16();
                    let headers = response
                        .headers()
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_owned()))
                        .collect();
                    let body =
                        collect(Box::pin(response.bytes_stream().map(|chunk| {
                            chunk.map(|b| b.to_vec()).map_err(|e| e.to_string())
                        })))
                        .await?;
                    Ok(asyncrt::OpOut::Native(HostReply::Fetch(api::Response {
                        status,
                        headers,
                        body,
                    })))
                });
            }
            HostCall::CallObject {
                object,
                method,
                body,
            } => {
                let class = &object.class;
                if !self.program.classes().contains(class) {
                    return Err("unknown durable class".into());
                }
                if body.len() > BODY_LIMIT {
                    return Err("object arguments exceed 1 MiB".into());
                }
                let name = object.id;
                if method.starts_with('_') || method == "alarm" {
                    return Err("unknown public method".into());
                }
                let key = namespace_key(&self.config.script_name, class);
                let id = durable_object_id_hex(&durable_object_id_for_name(&key, &name));
                let scope = format!("{class}:{id}");
                if entry.scope.as_deref() == Some(&scope) {
                    return Err("call same-object methods through self".into());
                }
                let gate = egress_gate_request(&entry.context, celld_logic::Channel::CellRpc);
                let order = Some(enter_call_order(entry.context.clone(), &scope));
                let request_id = next_do_request_id();
                let (cancel_sender, cancel) = tokio::sync::oneshot::channel();
                do_call_cancels()
                    .lock()
                    .unwrap()
                    .insert(request_id, cancel_sender);
                let mut cancel_guard = DoCallCancelGuard::new(request_id);
                let (reply, receive) = tokio::sync::oneshot::channel();
                let body = RequestBody::Bytes(body.into());
                let request = DoCallReq {
                    request_id: Some(request_id),
                    cancel: Some(cancel),
                    deliver_abort_to_handler: false,
                    scope,
                    name: Some(name),
                    url: format!("http://native.internal/{method}"),
                    method: "POST".into(),
                    body_guard: RequestBodyGuard::of(&body),
                    body,
                    headers: vec![("content-type".into(), "application/json".into())],
                    reply,
                    order,
                    parent: entry.trace,
                };
                asyncrt::enqueue(async move {
                    gated_channel_send(gate, &DO_CALL_TX, request, "no proxy channel").await?;
                    let response = receive
                        .await
                        .map_err(|_| "durable call dropped".to_string())?
                        .map_err(|e| e.to_string())?;
                    cancel_guard.disarm();
                    if response.body.len() > BODY_LIMIT {
                        return Err("object result exceeds 1 MiB".into());
                    }
                    Ok(asyncrt::OpOut::Native(HostReply::Object(api::Response {
                        status: response.status,
                        headers: response.headers,
                        body: response.body,
                    })))
                });
            }
            _ => return Err("unknown asynchronous capability".into()),
        }
        Ok(())
    }

    fn finish(&self, entry: &mut InFlight, result: Result<Step, Failure>) {
        // Drop/rollback while this worker's storage is installed. A suspended
        // invocation never retains an open SQL transaction.
        entry.native.take();
        let result = match storage_error(entry.scope.as_deref()) {
            Some(error) => Err(error.into()),
            None => result,
        };
        if let Err(error) = &result {
            entry.failure = Some(crate::telemetry::cap_error(error.to_string()));
        }
        entry.context.end_event();
        let gates = entry.context.take_arm_gates();
        let Some(answer) = entry.reply.take() else {
            return;
        };
        entry.gated_reply = match answer {
            Answer::Fetch(reply) => {
                let result = match result {
                    Ok(Step::Return(r)) => Ok(response(r, entry.gate_positions())),
                    Ok(Step::Call(_)) => Err(anyhow!(
                        "native execution completed with a pending host call"
                    )),
                    Err(error) => Ok(response(
                        self.program.error_response(error, entry.scope.is_some()),
                        entry.gate_positions(),
                    )),
                };
                send_answer_after_arm_gates(reply, result, gates)
            }
            Answer::Alarm(reply) => {
                entry.settle_alarm(result.is_ok(), result.is_err());
                let result = result
                    .map(|_| {
                        (
                            entry.scope.as_deref().and_then(storage::get_alarm),
                            entry.write_delta(),
                        )
                    })
                    .map_err(|error| {
                        fail_in_turn_error(anyhow::Error::from(error), entry.gate_positions())
                    });
                send_answer_after_arm_gates(reply, result, gates)
            }
            answer => answer.fail_with_arm_gates(anyhow!("invalid native answer"), gates),
        };
    }
    pub fn turn_cancel(&mut self, entry: &mut InFlight) -> Vec<Op> {
        let _cells = self.cells.install();
        entry.fail_cancelled(anyhow!("The client has disconnected"));
        entry.context.end_event();
        entry.abandon();
        vec![]
    }
    pub fn turn_poll(&mut self, _entry: &mut InFlight) -> Vec<Op> {
        vec![]
    }
    pub fn turn_finish_alarm(&mut self, entry: &mut InFlight) -> Option<u64> {
        let _cells = self.cells.install();
        entry.settle_alarm(false, false);
        entry.write_delta()
    }
}

fn storage_error(scope: Option<&str>) -> Option<String> {
    scope.and_then(storage::sql_critical_error)
}
fn body_limit() -> Failure {
    Failure {
        status: 413,
        code: "body_too_large".into(),
        message: "body exceeds 1 MiB".into(),
    }
}
fn response(
    r: api::Response,
    (write_position, observed_position): (Option<u64>, Option<u64>),
) -> HttpResponse {
    HttpResponse {
        status: r.status,
        headers: r.headers,
        body: r.body,
        stream: None,
        websocket: None,
        write_position,
        observed_position,
    }
}

/// Buffered native HTTP uses celld's pooled Rust client. Capture the event's
/// durability ticket before yielding; the driver cancels this future on hangup.
fn native_fetch(
    request: api::Request,
    context: Arc<IoContext>,
    trace: Option<crate::telemetry::TraceContext>,
) -> impl std::future::Future<Output = Result<reqwest::Response, String>> + Send {
    let client = HTTP.with(Clone::clone);
    let gate = egress_gate_request(&context, celld_logic::Channel::Fetch);
    let api::Request {
        url,
        method,
        mut headers,
        body,
    } = request;
    let child = trace.as_ref().map(crate::telemetry::child_context);
    if let Some(child) = child.as_ref() {
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("traceparent"))
        {
            headers.push(("traceparent".into(), crate::telemetry::traceparent(child)));
        }
    }
    let recording = trace.and_then(crate::telemetry::TraceContext::recording_ids);
    let child_recording = child.and_then(crate::telemetry::TraceContext::recording_ids);
    async move {
        let started = recording.map(|_| crate::telemetry::now_unix_us());
        let span = recording.zip(child_recording).map(|(parent, child)| {
            let mut span =
                crate::telemetry::Span::new(child, "fetch", crate::telemetry::KIND_CLIENT);
            span.parent_span_id = Some(parent.span_id);
            span.parent_remote = Some(false);
            span.url = Some(url.clone());
            span
        });
        let result = async {
            await_egress_gate(gate).await?;
            let method = reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|_| "invalid HTTP method".to_string())?;
            let timeout = crate::env_vars::positive_or("CELLD_FETCH_TIMEOUT_S", 120)
                .expect("validated CELLD_FETCH_TIMEOUT_S");
            let empty_body_needs_length = body.is_empty()
                && !headers.iter().any(|(name, _)| {
                    name.eq_ignore_ascii_case("content-length")
                        || name.eq_ignore_ascii_case("transfer-encoding")
                });
            let mut request = client
                .request(method, url)
                .timeout(Duration::from_secs(timeout))
                .body(body);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            if empty_body_needs_length {
                request = request.header(reqwest::header::CONTENT_LENGTH, 0);
            }
            request.send().await.map_err(|e| format!("fetch: {e}"))
        }
        .await;
        if let Some(mut span) = span {
            span.start_unix_us = started.unwrap_or_default();
            span.duration_us = crate::telemetry::now_unix_us() - span.start_unix_us;
            span.ok = result.is_ok();
            span.http_status = result.as_ref().ok().map(|r| r.status().as_u16());
            span.error = result.as_ref().err().cloned();
            crate::telemetry::record(span);
        }
        result
    }
}
async fn collect(mut stream: HttpChunkStream) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > BODY_LIMIT - body.len() {
            return Err("body exceeds 1 MiB".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
