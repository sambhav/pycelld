//! Operator-owned Python policies, isolated from worker modules and capabilities.
use crate::{FetchContext, FetchDecision, FetchRequest};
use celld_runtime::Response;
use monty::{MontyRun, RunProgress};
use monty_types::{CompileOptions, MontyObject, PrintWriter, ResourceLimits, ResourceTracker};
use ruff_python_ast::Stmt;
use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_BYTES: usize = 1024 * 1024;

/// Immutable, precompiled operator policy. Every decision starts fresh globals.
/// No worker extensions, environment, filesystem, or external calls are installed.
#[derive(Clone)]
pub struct PythonNetworkPolicy {
    // MontyRun is Send but not Sync. Only snapshot cloning holds this lock;
    // interpreter execution takes place independently outside it.
    runner: Arc<Mutex<MontyRun>>,
}

impl PythonNetworkPolicy {
    /// Compile a synchronous `def policy(request, context)` function.
    pub fn compile(source: &str) -> celld_runtime::Result<Self> {
        if source.len() > MAX_BYTES {
            return Err("network policy source exceeds 1 MiB".into());
        }
        let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
        let definitions: Vec<_> = parsed
            .syntax()
            .body
            .iter()
            .filter_map(|statement| {
                if let Stmt::FunctionDef(def) = statement {
                    (def.name.as_str() == "policy").then_some(def)
                } else {
                    None
                }
            })
            .collect();
        let [def] = definitions.as_slice() else {
            return Err(
                "network policy must define exactly one policy(request, context) function".into(),
            );
        };
        if def.is_async
            || !def.decorator_list.is_empty()
            || def.parameters.vararg.is_some()
            || def.parameters.kwarg.is_some()
            || !def.parameters.kwonlyargs.is_empty()
            || def.parameters.posonlyargs.len() + def.parameters.args.len() != 2
        {
            return Err("network policy must be synchronous with two positional parameters".into());
        }
        let runner = MontyRun::new(
            format!("{source}\npolicy(_celld_policy_request, _celld_policy_context)\n"),
            "network_policy.py",
            vec![
                "_celld_policy_request".into(),
                "_celld_policy_context".into(),
            ],
            CompileOptions::default(),
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            runner: Arc::new(Mutex::new(runner)),
        })
    }

    /// Read one bounded snapshot. File changes take effect only after a restart
    /// or explicit host reconstruction; a failed replacement never changes it.
    pub fn load(path: impl AsRef<Path>) -> celld_runtime::Result<Self> {
        let mut source = String::new();
        std::fs::File::open(path.as_ref())
            .map_err(|e| format!("cannot open network policy: {e}"))?
            .take((MAX_BYTES + 1) as u64)
            .read_to_string(&mut source)
            .map_err(|e| format!("cannot read network policy: {e}"))?;
        Self::compile(&source)
    }

    pub fn decide(&self, context: &FetchContext, request: FetchRequest) -> FetchDecision {
        match self.evaluate(context, request) {
            Ok(decision) => decision,
            // Do not expose policy source, credentials, or exception messages to workers.
            Err(_) => FetchDecision::Deny("network policy failed; outbound request denied".into()),
        }
    }

    fn evaluate(
        &self,
        context: &FetchContext,
        mut request: FetchRequest,
    ) -> Result<FetchDecision, String> {
        crate::network::validate(&request)?;
        let input = serde_json::json!({
            "url": request.url.as_str(), "scheme": request.url.scheme(),
            "host": request.url.host_str(), "port": request.url.port_or_known_default(),
            "method": request.method, "headers": request.headers,
        });
        // Bound conversion as well as the returned values. Byte bodies stay native.
        let metadata = serde_json::json!({
            "request_url": context.request_url, "alarm": context.alarm,
            "object": context.object.as_ref().map(|(class, id)| serde_json::json!({"class": class, "id": id})),
            "execution": context.execution, "limits": context.limits,
        });
        let max_bytes = context.limits.max_payload_bytes.min(MAX_BYTES);
        if input.to_string().len() + metadata.to_string().len() + request.body.len() > max_bytes {
            return Err("policy input exceeds 1 MiB".into());
        }
        let MontyObject::Dict(values) = crate::value::from_json(&input) else {
            unreachable!()
        };
        let values = values
            .into_iter()
            .chain(std::iter::once((
                MontyObject::String("body".into()),
                MontyObject::Bytes(request.body.clone()),
            )))
            .collect();
        let runner = self
            .runner
            .lock()
            .map_err(|_| "policy snapshot unavailable")?
            .clone();
        let progress = runner
            .start(
                vec![
                    MontyObject::Dict(values),
                    crate::value::from_json(&metadata),
                ],
                ResourceTracker::new(ResourceLimits {
                    max_duration: Some(Duration::from_millis(context.limits.cpu_ms.min(10))),
                    max_recursion_depth: 50,
                    ..Default::default()
                }),
                PrintWriter::Disabled,
            )
            .map_err(|e| e.to_string())?;
        let RunProgress::Complete(result) = progress else {
            return Err("policy cannot perform host calls or suspend".into());
        };
        if result.deep_host_size() > max_bytes {
            return Err("policy result exceeds payload limit".into());
        }
        let MontyObject::Dict(result) = result else {
            return Err("policy must return a dictionary".into());
        };
        let mut fields = BTreeMap::new();
        for (key, value) in result {
            let MontyObject::String(key) = key else {
                return Err("policy keys must be strings".into());
            };
            fields.insert(key, value);
        }
        let action = string(fields.remove("action").ok_or("missing policy action")?)?;
        let decision = match action.as_str() {
            "forward" => {
                if let Some(value) = fields.remove("url") {
                    request.url = url::Url::parse(&string(value)?).map_err(|e| e.to_string())?;
                }
                if let Some(value) = fields.remove("method") {
                    request.method = string(value)?;
                    if !token(&request.method) {
                        return Err("invalid HTTP method".into());
                    }
                }
                if let Some(value) = fields.remove("headers") {
                    request.headers = headers(value)?;
                }
                if let Some(value) = fields.remove("body") {
                    request.body = body(value)?;
                }
                crate::network::validate(&request)?;
                FetchDecision::Forward(request)
            }
            "respond" => {
                let status = match fields.remove("status") {
                    Some(MontyObject::Int(status)) if (200..=599).contains(&status) => {
                        status as u16
                    }
                    _ => return Err("response status must be between 200 and 599".into()),
                };
                let headers = fields
                    .remove("headers")
                    .map(headers)
                    .transpose()?
                    .unwrap_or_default();
                let body = fields
                    .remove("body")
                    .map(body)
                    .transpose()?
                    .unwrap_or_default();
                FetchDecision::Respond(Response {
                    status,
                    headers,
                    body,
                })
            }
            "deny" => FetchDecision::Deny(
                fields
                    .remove("reason")
                    .map(string)
                    .transpose()?
                    .unwrap_or_else(|| "outbound request denied by network policy".into()),
            ),
            _ => return Err("unknown policy action".into()),
        };
        if !fields.is_empty() {
            return Err("unexpected policy fields".into());
        }
        Ok(decision)
    }
}

fn string(value: MontyObject) -> Result<String, String> {
    if let MontyObject::String(value) = value {
        Ok(value)
    } else {
        Err("expected string".into())
    }
}
fn body(value: MontyObject) -> Result<Vec<u8>, String> {
    match value {
        MontyObject::Bytes(value) => Ok(value),
        MontyObject::String(value) => Ok(value.into_bytes()),
        _ => Err("body must be str or bytes".into()),
    }
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
fn headers(value: MontyObject) -> Result<Vec<(String, String)>, String> {
    let MontyObject::List(values) = value else {
        return Err("headers must be a list of pairs".into());
    };
    values
        .into_iter()
        .map(|pair| {
            let (MontyObject::List(mut pair) | MontyObject::Tuple(mut pair)) = pair else {
                return Err("header must be a pair".into());
            };
            if pair.len() != 2 {
                return Err("header must be a pair".into());
            }
            let value = string(pair.pop().unwrap())?;
            let name = string(pair.pop().unwrap())?;
            if !token(&name) || value.bytes().any(|b| (b < 32 && b != b'\t') || b == 127) {
                return Err("invalid HTTP header".into());
            }
            Ok((name, value))
        })
        .collect()
}

impl crate::Monty {
    /// Append an isolated Python policy to the existing Rust middleware chain.
    pub fn with_network_policy(self, policy: PythonNetworkPolicy) -> Self {
        self.with_fetch_middleware(move |context, request| policy.decide(context, request))
    }
}
