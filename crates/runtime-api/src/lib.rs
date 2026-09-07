//! Native runtime contract. The host owns I/O, cell authority and durability;
//! implementations own compilation, language values and suspended executions.
//! No interpreter, executor, database, or JavaScript types cross this boundary.
use serde_json::Value;

pub type Result<T> = std::result::Result<T, Failure>;

pub struct Descriptor {
    pub extension: &'static str,
    pub main_module: &'static str,
    pub artifact_prefix: &'static str,
    pub required_feature: &'static str,
}

pub trait Runtime: Send + Sync {
    fn descriptor(&self) -> &Descriptor;
    fn compile(&self, source: &str) -> Result<Box<dyn Program>>;
    fn types(&self) -> &str;
}

/// Cached compilation is cloned once per worker slot. Programs and executions
/// may move between threads, but are never entered concurrently.
pub trait Program: Send {
    fn fork(&self) -> Box<dyn Program>;
    fn classes(&self) -> &[String];
    fn start(&self, call: Invocation) -> Result<(Box<dyn Execution>, Step)>;
    fn error_response(&self, error: Failure, durable: bool) -> Response;
}

/// One suspended invocation. Dropping it cancels language execution; the host
/// independently cancels its I/O and releases gates/transactions. Implementations
/// must bound interpreter work before returning each Step.
pub trait Execution: Send {
    fn resume(&mut self, reply: HostReply) -> Result<Step>;
}

pub enum Step {
    Return(Response),
    Call(HostCall),
}

pub struct Invocation {
    pub request: Request,
    pub object: Option<Object>,
    pub alarm: bool,
    pub env: Value,
}

pub struct Object {
    pub class: String,
    pub id: String,
}

pub struct Request {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Storage values are JSON. HTTP and durable-call bodies stay native byte
/// buffers; their encoding belongs to the language runtime, never to the host.
pub enum HostCall {
    Get(String),
    Put(String, Value),
    Delete(String),
    Clear,
    List {
        prefix: String,
        limit: usize,
        reverse: bool,
    },
    Sql {
        query: String,
        bindings: Vec<Value>,
    },
    GetAlarm,
    SetAlarm(i64),
    DeleteAlarm,
    BeginTransaction,
    CommitTransaction,
    RollbackTransaction,
    Sync,
    Fetch(Request),
    CallObject {
        object: Object,
        method: String,
        body: Vec<u8>,
    },
    Sleep(std::time::Duration),
    Now,
    Uuid,
    Log(String),
}

pub enum HostReply {
    Value(Value),
    Timestamp(Option<i64>),
    Fetch(Response),
    Object(Response),
    Error(String),
}

#[derive(Debug)]
pub struct Failure {
    pub status: u16,
    pub code: String,
    pub message: String,
}
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
        message.to_owned().into()
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Failure {}
