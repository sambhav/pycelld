//! Scoped native capabilities. Only the worker driver can choose the cell;
//! dropping a turn rolls back its unfinished transactions.
use crate::storage;
use celld_runtime::{HostCall, HostReply};
use serde_json::{Value, json};

pub(crate) struct Host {
    pub scope: Option<String>,
    transactions: usize,
}
impl Host {
    pub fn new(scope: Option<String>) -> Self {
        Self {
            scope,
            transactions: 0,
        }
    }
    pub fn in_transaction(&self) -> bool {
        self.transactions > 0
    }
    pub fn handles(call: &HostCall) -> bool {
        !matches!(
            call,
            HostCall::Sync | HostCall::Sleep(_) | HostCall::Fetch(_) | HostCall::CallObject { .. }
        )
    }
    pub fn call(&mut self, call: HostCall) -> Result<(HostReply, Option<i64>), String> {
        use HostCall::*;
        match call {
            Filesystem(call) => {
                let reply = match self.scope.as_deref() {
                    Some(scope) => storage::filesystem::call(scope, call),
                    None => Err(celld_runtime::filesystem::FsError::new(
                        celld_runtime::filesystem::FsErrorKind::Permission,
                        "filesystem access requires a durable object",
                    )),
                };
                return Ok((HostReply::Filesystem(reply), None));
            }
            Now => {
                return Ok((
                    HostReply::Timestamp(Some(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_err(|e| e.to_string())?
                            .as_millis() as i64,
                    )),
                    None,
                ));
            }
            Uuid => return Ok((HostReply::Value(json!(new_uuid()?)), None)),
            Log(message) => {
                tracing::info!(target: "native", "{message}");
                return Ok((HostReply::Value(Value::Null), None));
            }
            _ => {}
        }
        let scope = self
            .scope
            .as_deref()
            .ok_or("storage requires a durable object")?;
        let mut alarm = None;
        let value = match call {
            Get(key) => {
                let value = storage::get_stored(scope, &key).map_err(|e| e.to_string())?;
                json!({"found":value.is_some(),"value":value.map(decode).transpose()?})
            }
            Put(key, value) => {
                storage::put_json(scope, &key, &value).map_err(|e| e.to_string())?;
                Value::Null
            }
            Delete(key) => json!(storage::delete(scope, &key).map_err(|e| e.to_string())?),
            Clear => {
                storage::delete_all_with_alarm(scope, false).map_err(|e| e.to_string())?;
                Value::Null
            }
            List {
                prefix,
                limit,
                reverse,
            } => {
                if limit > 1000 {
                    return Err("list limit must be 0-1000".into());
                }
                Value::Object(
                    storage::list_stored_with_options(
                        scope,
                        None,
                        None,
                        None,
                        Some(&prefix),
                        Some(limit),
                        reverse,
                    )
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|(k, v)| Ok((k, decode(v)?)))
                    .collect::<Result<_, String>>()?,
                )
            }
            Sql { query, bindings } => {
                let (columns, rows, _) = storage::sql_exec(scope, &query, &bindings)?;
                json!(
                    rows.into_iter()
                        .map(|row| columns
                            .iter()
                            .cloned()
                            .zip(row)
                            .collect::<serde_json::Map<_, _>>())
                        .collect::<Vec<_>>()
                )
            }
            GetAlarm => return Ok((HostReply::Timestamp(storage::get_alarm(scope)), None)),
            SetAlarm(at) => {
                alarm = storage::set_alarm(scope, at).map_err(|e| e.to_string())?;
                Value::Null
            }
            DeleteAlarm => {
                storage::delete_alarm(scope).map_err(|e| e.to_string())?;
                Value::Null
            }
            BeginTransaction => {
                if self.transactions >= 16 {
                    return Err("transaction nesting limit exceeded".into());
                }
                storage::transaction_control(
                    scope,
                    "start",
                    self.transactions > 0,
                    &format!("cells_tx_{}", self.transactions),
                )?;
                self.transactions += 1;
                Value::Null
            }
            CommitTransaction | RollbackTransaction => {
                let depth = self
                    .transactions
                    .checked_sub(1)
                    .ok_or("no open transaction")?;
                alarm = storage::transaction_control(
                    scope,
                    if matches!(call, CommitTransaction) {
                        "commit"
                    } else {
                        "rollback"
                    },
                    depth > 0,
                    &format!("cells_tx_{depth}"),
                )?;
                self.transactions = depth;
                Value::Null
            }
            _ => return Err("asynchronous capability requires the worker driver".into()),
        };
        Ok((HostReply::Value(value), alarm))
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        if let Some(scope) = &self.scope {
            while self.transactions > 0 {
                self.transactions -= 1;
                let _ = storage::transaction_control(
                    scope,
                    "rollback",
                    self.transactions > 0,
                    &format!("cells_tx_{}", self.transactions),
                );
            }
        }
    }
}
fn decode(value: storage::StoredValue) -> Result<Value, String> {
    match value {
        storage::StoredValue::LegacyJson(json) => {
            serde_json::from_str(&json).map_err(|e| e.to_string())
        }
        storage::StoredValue::V8(_) => Err(
            "native storage expects JSON; this key contains a JavaScript structured clone".into(),
        ),
    }
}
pub(super) fn new_uuid() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
