//! A captured, immutable host identity accompanies every native diagnostic.
use crate::telemetry;
use celld_runtime::{
    ExecutionMetadata,
    observability::{Diagnostic, Observer},
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn observer(
    metadata: &ExecutionMetadata,
    object: Option<&celld_runtime::Object>,
    trace: Option<telemetry::TraceContext>,
) -> Arc<dyn Observer> {
    // Do not copy principal claims, request headers, variables, or secrets to logs.
    let identity = json!({
        "worker_id":metadata.worker_id,
        "deployment_id":metadata.deployment_id,
        "invocation_id":metadata.invocation_id,
        "runtime_instance_id":metadata.runtime_instance_id,
        "root_invocation_id":metadata.root_invocation_id,
        "parent_invocation_id":metadata.parent_invocation_id,
        "object_class":object.map(|o| &o.class),
        "object_id":object.map(|o| &o.id),
        "trace_id":trace.map(|t| t.trace_id.iter().map(|b| format!("{b:02x}")).collect::<String>()),
        "span_id":trace.map(|t| t.span_id.iter().map(|b| format!("{b:02x}")).collect::<String>()),
    });
    // Logical object names can originate in request data and be arbitrarily long.
    let mut identity = identity;
    for value in identity.as_object_mut().unwrap().values_mut() {
        if let Some(text) = value.as_str() {
            if text.len() > 128 {
                let mut end = 128;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                *value = Value::String(format!("{}…", &text[..end]));
            }
        }
    }
    Arc::new(move |event: Diagnostic| {
        let ids = trace.and_then(telemetry::TraceContext::recording_ids);
        match event {
            Diagnostic::Log {
                level,
                message,
                fields,
            } => {
                let body = log_body(&identity, &level, message, fields);
                match level.as_str() {
                    "error" => tracing::error!(target: "native", "{body}"),
                    "warn" => tracing::warn!(target: "native", "{body}"),
                    "debug" => tracing::debug!(target: "native", "{body}"),
                    _ => tracing::info!(target: "native", "{body}"),
                }
                // Respect the invocation's sampling decision, including unsampled parents.
                if let Some(ids) = ids {
                    telemetry::record_log(telemetry::Log {
                        trace_id: Some(ids.trace_id),
                        span_id: Some(ids.span_id),
                        time_unix_us: telemetry::now_unix_us(),
                        body,
                    });
                }
            }
            Diagnostic::Span {
                name,
                fields,
                start_unix_us,
                duration_us,
                ok,
            } => {
                if let Some((parent, child)) = ids.zip(
                    trace
                        .map(|t| telemetry::child_context(&t))
                        .and_then(telemetry::TraceContext::recording_ids),
                ) {
                    let mut span = telemetry::Span::new(child, "python", telemetry::KIND_INTERNAL);
                    span.name = name.into();
                    span.parent_span_id = Some(parent.span_id);
                    span.parent_remote = Some(false);
                    span.start_unix_us = start_unix_us;
                    span.duration_us = duration_us;
                    span.ok = ok;
                    span.request_id = identity["invocation_id"].as_str().map(str::to_owned);
                    span.attributes = Some(json!({"execution":identity,"fields":fields}));
                    telemetry::record(span);
                }
            }
        }
    })
}

fn log_body(identity: &Value, level: &str, message: String, fields: Value) -> String {
    let mut record = json!({"level":level,"message":message,"fields":fields,"execution":identity});
    if record.to_string().len() > telemetry::LOG_BODY_CAP {
        record["truncated"] = json!(true);
        record["fields"] = json!({"truncated":true});
        while record.to_string().len() > telemetry::LOG_BODY_CAP {
            let text = record["message"].as_str().unwrap_or("");
            if text.is_empty() {
                break;
            }
            let mut end = text.len() / 2;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            record["message"] = Value::String(text[..end].to_owned());
        }
    }
    record.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_fields_cannot_replace_identity_and_truncation_keeps_json_valid() {
        let identity = json!({"invocation_id":"trusted","object_id":"object-a"});
        let body = log_body(
            &identity,
            "info",
            "\u{0001}".repeat(9000),
            json!({"execution":{"invocation_id":"forged"}}),
        );
        assert!(body.len() <= telemetry::LOG_BODY_CAP);
        let record: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(record["execution"]["invocation_id"], "trusted");
        assert_eq!(record["execution"]["object_id"], "object-a");
        assert_eq!(record["truncated"], true);
    }
}
