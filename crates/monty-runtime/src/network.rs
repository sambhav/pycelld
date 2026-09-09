//! Host-controlled outbound HTTP. No middleware means no network access.
use celld_runtime::{Request, Response};
use serde_json::Value;
use std::sync::Arc;

/// Invocation metadata supplied by the host, never by fetch arguments.
pub struct FetchContext {
    pub execution: celld_runtime::ExecutionMetadata,
    pub limits: celld_runtime::ExecutionLimits,
    pub request_url: String,
    pub env: Value,
    /// Durable class identity and ID, or `None` for a stateless handler.
    pub object: Option<(String, String)>,
    pub alarm: bool,
}

/// A parsed HTTP(S) request. Bodies remain native byte buffers.
pub struct FetchRequest {
    pub url: url::Url,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub enum FetchDecision {
    /// Pass this request to the next middleware, then celld's HTTP transport.
    Forward(FetchRequest),
    /// Complete the call without network I/O or running later middleware.
    Respond(Response),
    /// Raise a catchable Python RuntimeError without network I/O.
    Deny(String),
}

type Middleware = dyn Fn(&FetchContext, FetchRequest) -> FetchDecision + Send + Sync;

#[derive(Clone, Default)]
pub(crate) struct Network(pub(crate) Arc<Vec<Arc<Middleware>>>);
impl Network {
    pub(crate) fn intercept(&self, context: &FetchContext, request: Request) -> FetchDecision {
        if self.0.is_empty() {
            return FetchDecision::Deny("outbound HTTP is disabled; the host must install fetch middleware".into());
        }
        let url = match url::Url::parse(&request.url) {
            Ok(url) => url,
            Err(_) => return FetchDecision::Deny("invalid fetch URL".into()),
        };
        let mut request = FetchRequest {
            url,
            method: request.method,
            headers: request.headers,
            body: request.body,
        };
        for middleware in self.0.iter() {
            if let Err(error) = validate(&request) {
                return FetchDecision::Deny(error.into());
            }
            match middleware(context, request) {
                FetchDecision::Forward(next) => request = next,
                result => return result,
            }
        }
        match validate(&request) {
            Ok(()) => FetchDecision::Forward(request),
            Err(error) => FetchDecision::Deny(error.into()),
        }
    }
}

fn validate(request: &FetchRequest) -> Result<(), &'static str> {
    if !matches!(request.url.scheme(), "http" | "https") || request.url.host().is_none() {
        return Err("fetch requires an absolute HTTP(S) URL");
    }
    if !request.url.username().is_empty() || request.url.password().is_some() {
        return Err("fetch URL credentials are unsupported; use headers");
    }
    if request.body.len() > 1024 * 1024 {
        return Err("fetch body exceeds 1 MiB");
    }
    Ok(())
}

impl crate::Monty {
    /// Append host middleware, evaluated in registration order for every fetch.
    /// With none installed, all outbound HTTP is denied. Forwarding still obeys
    /// celld's egress policy, cancellation and durability gates. Redirects are
    /// returned to Python; fetching a Location requires a fresh policy decision.
    /// Callbacks must be bounded and nonblocking, like native Python extensions.
    pub fn with_fetch_middleware(
        mut self,
        middleware: impl Fn(&FetchContext, FetchRequest) -> FetchDecision + Send + Sync + 'static,
    ) -> Self {
        Arc::make_mut(&mut self.network.0).push(Arc::new(middleware));
        self
    }
}
