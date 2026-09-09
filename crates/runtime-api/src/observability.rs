//! Best-effort diagnostics. A sink is scoped by the host to one invocation;
//! runtime code cannot select another invocation's identity or trace.
use serde_json::Value;

#[derive(Clone, Debug)]
pub enum Diagnostic {
    Log {
        level: String,
        message: String,
        fields: Value,
    },
    Span {
        name: String,
        fields: Value,
        start_unix_us: i64,
        duration_us: i64,
        ok: bool,
    },
}

/// Implementations must enqueue with a bounded, nonblocking operation (or drop).
/// Exporter delivery and retries must run independently of worker execution.
pub trait Observer: Send + Sync {
    fn emit(&self, diagnostic: Diagnostic);
}
impl<F: Fn(Diagnostic) + Send + Sync> Observer for F {
    fn emit(&self, diagnostic: Diagnostic) {
        self(diagnostic)
    }
}
