//! Native Python compilation, values, capabilities and suspended executions.
mod exports;
mod filesystem;
mod extensions;
mod modules;
mod network;
mod package;
mod runtime;
mod value;
pub use extensions::{PythonModule, PythonResult};
pub use monty_types::{ExcType, MontyException as PythonError, MontyObject as PythonValue};
pub use runtime::Monty;
pub use network::{FetchContext, FetchDecision, FetchRequest};

/// Editor definitions for the built-in Python module.
pub const TYPES: &str = include_str!("celld.pyi");
use monty::{FunctionCall, RunProgress};
use monty_types::{ExtFunctionResult, MontyException, MontyObject, PrintWriter};
use serde_json::{Value, json};

#[derive(Debug)]
pub(crate) struct Failure {
    pub status: u16,
    pub code: String,
    pub message: String,
}
impl Failure {
    pub fn arguments(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            code: "invalid_arguments".into(),
            message: message.into(),
        }
    }
    pub fn python(error: MontyException) -> Self {
        Self {
            status: 500,
            code: error.exc_type().to_string(),
            message: error.message().unwrap_or("Python execution failed").into(),
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Failure {}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            status: 500,
            code: "runtime_error".into(),
            message,
        }
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

pub const CAPABILITIES: &[&str] = &[
    "storage.get",
    "storage.put",
    "storage.delete",
    "storage.list",
    "storage.delete_all",
    "storage.sql",
    "storage.get_alarm",
    "storage.sync",
    "storage.set_alarm",
    "storage.delete_alarm",
    "storage.transaction_begin",
    "storage.transaction_commit",
    "storage.transaction_rollback",
    "object.call",
    "fetch",
    "sleep",
    "now",
    "uuid",
    "log",
];

pub(crate) struct Session {
    pending: Option<FunctionCall>,
    pending_os: Option<(monty::OsCall, filesystem::ResultType)>,
    filesystem_call: Option<celld_runtime::filesystem::FsCall>,
    calls: usize,
    rpc: bool,
    response: Option<value::HttpResponse>,
    extensions: std::sync::Arc<extensions::Extensions>,
}
impl Session {
    pub fn start(
        function: &exports::Function,
        args: &Value,
        context: &Value,
    ) -> Result<(Self, Value), Failure> {
        let mut session = Self {
            pending: None,
            pending_os: None,
            filesystem_call: None,
            calls: 0,
            rpc: function.rpc,
            response: None,
            extensions: function.extensions.clone(),
        };
        let event = session.advance(function.start(args, context)?)?;
        Ok((session, event))
    }
    pub fn take_response(&mut self) -> Option<value::HttpResponse> {
        self.response.take()
    }

    /// Take an outbound binary body without a Python list or JSON byte array.
    pub fn take_fetch_body(&mut self) -> Option<Vec<u8>> {
        let call = self.pending.as_mut()?;
        if !matches!(call.args.first(), Some(MontyObject::String(op)) if op == "fetch") {
            return None;
        }
        let body = call.args.get_mut(4)?;
        if !matches!(body, MontyObject::Bytes(_)) {
            return None;
        }
        match std::mem::replace(body, MontyObject::None) {
            MontyObject::Bytes(body) => Some(body),
            _ => unreachable!(),
        }
    }

    pub fn take_filesystem_call(&mut self) -> Result<celld_runtime::filesystem::FsCall, Failure> {
        self.filesystem_call.take().ok_or_else(|| "no pending filesystem call".into())
    }
    pub fn resume_filesystem(&mut self, reply: celld_runtime::filesystem::FsResult) -> Result<Value, Failure> {
        let (call, result) = self.pending_os.take().ok_or("no pending OS call")?;
        let progress = call.resume(filesystem::response(reply, result), PrintWriter::Disabled)
            .map_err(Failure::python)?;
        self.advance(progress)
    }
    pub fn resume(&mut self, reply: Value) -> Result<Value, Failure> {
        if self.pending_os.is_some() {
            let message = reply["error"].as_str().unwrap_or("filesystem host call failed");
            return self.resume_filesystem(Err(celld_runtime::filesystem::FsError::new(
                celld_runtime::filesystem::FsErrorKind::Io, message)));
        }
        let call = self.pending.take().ok_or("session is not suspended")?;
        if reply.to_string().len() > 1024 * 1024 {
            return Err("host result exceeds 1 MiB".into());
        }
        let reply = if let Some(error) = reply.get("error") {
            let kind = error["code"]
                .as_str()
                .and_then(|code| code.parse().ok())
                .unwrap_or(ExcType::RuntimeError);
            let message = error
                .as_str()
                .or_else(|| error["message"].as_str())
                .unwrap_or("host call failed");
            ExtFunctionResult::Error(MontyException::new(kind, Some(message.into())))
        } else if let Some(wire) = reply.get("wire").and_then(Value::as_str) {
            let mut value: MontyObject = serde_json::from_str(wire).map_err(|e| e.to_string())?;
            value::host_value(&mut value);
            ExtFunctionResult::Return(value)
        } else {
            ExtFunctionResult::Return(value::from_json(&reply["result"]))
        };
        let progress = call
            .resume(reply, PrintWriter::Disabled)
            .map_err(Failure::python)?;
        self.advance(progress)
    }
    /// Resume native HTTP without expanding a body into JSON numbers. The
    /// buffer moves directly into Monty's bytes value at the VM boundary.
    pub fn resume_fetch(
        &mut self,
        status: u16,
        headers: Value,
        body: Vec<u8>,
    ) -> Result<Value, Failure> {
        if body.len() + headers.to_string().len() > 1024 * 1024 {
            return Err("host result exceeds 1 MiB".into());
        }
        let call = self.pending.take().ok_or("session is not suspended")?;
        if !matches!(call.args.first(), Some(MontyObject::String(op)) if op == "fetch") {
            return Err("fetch result delivered to a different capability".into());
        }
        let value = MontyObject::Dict(
            vec![
                (
                    MontyObject::String("status".into()),
                    MontyObject::Int(i64::from(status)),
                ),
                (
                    MontyObject::String("headers".into()),
                    value::from_json(&headers),
                ),
                (MontyObject::String("body".into()), MontyObject::Bytes(body)),
            ]
            .into(),
        );
        let progress = call
            .resume(ExtFunctionResult::Return(value), PrintWriter::Disabled)
            .map_err(Failure::python)?;
        self.advance(progress)
    }
    fn advance(&mut self, mut progress: RunProgress) -> Result<Value, Failure> {
        loop {
            if let RunProgress::FunctionCall(call) = &progress {
                if let Some((arity, callback)) = self.extensions.functions.get(&call.function_name)
                {
                    self.calls += 1;
                    if self.calls > 10_000 {
                        return Err("host call limit exceeded".into());
                    }
                    let RunProgress::FunctionCall(mut call) = progress else {
                        unreachable!()
                    };
                    let reply = if call.object_id.is_some()
                        || !call.kwargs.is_empty()
                        || call.args.len() != *arity
                    {
                        Err(PythonError::new(
                            ExcType::TypeError,
                            Some("invalid native function arguments".into()),
                        ))
                    } else if call.args.iter().fold(0usize, |size, arg| {
                        size.saturating_add(arg.deep_host_size())
                    }) > 1024 * 1024
                    {
                        Err(PythonError::new(
                            ExcType::ValueError,
                            Some("native function arguments exceed 1 MiB".into()),
                        ))
                    } else {
                        callback(std::mem::take(&mut call.args)).and_then(|value| {
                            if value.deep_host_size() > 1024 * 1024 {
                                Err(PythonError::new(
                                    ExcType::ValueError,
                                    Some("native function result exceeds 1 MiB".into()),
                                ))
                            } else {
                                Ok(value)
                            }
                        })
                    };
                    progress = call
                        .resume(
                            match reply {
                                Ok(value) => ExtFunctionResult::Return(value),
                                Err(error) => ExtFunctionResult::Error(error),
                            },
                            PrintWriter::Disabled,
                        )
                        .map_err(Failure::python)?;
                    continue;
                }
            }
            if let RunProgress::OsCall(mut call) = progress {
                self.calls += 1;
                if self.calls > 10_000 { return Err("host call limit exceeded".into()); }
                let operation = std::mem::replace(&mut call.function_call, monty_types::OsFunctionCall::GetEnviron);
                match filesystem::request(operation) {
                    Ok((request, result)) => {
                        self.filesystem_call = Some(request);
                        self.pending_os = Some((call, result));
                        return Ok(json!({"done":false,"operation":"filesystem"}));
                    }
                    Err(error) => {
                        progress = call.resume(ExtFunctionResult::Error(error), PrintWriter::Disabled)
                            .map_err(Failure::python)?;
                        continue;
                    }
                }
            }
            return match progress {
                RunProgress::Complete(result) => {
                    // A caller needs one representation, never both. RPC retains
                    // native type information; HTTP serializes the Python value once.
                    if self.rpc {
                        // Validate even for RPC: iterators and arbitrary instances
                        // have no remote value contract in this interface.
                        value::to_json(&result)?;
                        let wire = serde_json::to_string(&result).map_err(|e| e.to_string())?;
                        if wire.len() > 1024 * 1024 {
                            return Err("result exceeds 1 MiB".into());
                        }
                        Ok(json!({"done":true,"wire":wire}))
                    } else {
                        self.response = Some(value::response(&result)?);
                        Ok(json!({"done":true}))
                    }
                }
                RunProgress::FunctionCall(call) => {
                    self.calls += 1;
                    if self.calls > 10_000 {
                        return Err("host call limit exceeded".into());
                    }
                    if call.function_name != "_celld_host"
                        || !call.kwargs.is_empty()
                        || call.object_id.is_some()
                    {
                        return Err("unregistered host function".into());
                    }
                    let Some(MontyObject::String(operation)) = call.args.first() else {
                        return Err("invalid host operation".into());
                    };
                    if !CAPABILITIES.contains(&operation.as_str()) {
                        return Err("unknown capability".into());
                    }
                    let args = if operation == "object.call" {
                        let [_, class, id, method, args] = call.args.as_slice() else {
                            return Err("invalid object call".into());
                        };
                        json!([
                            value::to_json(class)?,
                            value::to_json(id)?,
                            value::to_json(method)?,
                            serde_json::to_string(args).map_err(|e| e.to_string())?
                        ])
                    } else if operation == "fetch" {
                        let [_, url, method, headers, body] = call.args.as_slice() else {
                            return Err("invalid fetch call".into());
                        };
                        json!([
                            value::to_json(url)?,
                            value::to_json(method)?,
                            value::to_json(headers)?,
                            if matches!(body, MontyObject::Bytes(_)) {
                                Value::Null
                            } else {
                                value::to_json(body)?
                            }
                        ])
                    } else {
                        Value::Array(
                            call.args[1..]
                                .iter()
                                .map(value::to_json)
                                .collect::<Result<_, _>>()?,
                        )
                    };
                    let body_size = if operation == "fetch" {
                        match call.args.get(4) {
                            Some(MontyObject::Bytes(body)) => body.len(),
                            _ => 0,
                        }
                    } else {
                        0
                    };
                    if args.to_string().len() + body_size > 1024 * 1024 {
                        return Err("host arguments exceed 1 MiB".into());
                    }
                    let event = json!({"done":false,"operation":operation,"args":args});
                    self.pending = Some(call);
                    Ok(event)
                }
                _ => Err("only registered celld capabilities may suspend execution".into()),
            };
        }
    }
}

#[cfg(test)]
#[path = "../tests/runtime.rs"]
mod tests;
