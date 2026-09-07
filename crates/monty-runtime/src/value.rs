//! Conversion happens once at the native boundary, never through Python JSON.
use monty_types::MontyObject as M;
use serde_json::{Value, json};

pub fn from_json(value: &Value) -> M {
    match value {
        Value::Null => M::None,
        Value::Bool(v) => M::Bool(*v),
        Value::Number(v) => {
            if let Some(v) = v.as_i64() {
                M::Int(v)
            } else if !v.to_string().contains(['.', 'e', 'E']) {
                M::BigInt(v.to_string().parse().expect("validated JSON integer"))
            } else {
                M::Float(v.as_f64().unwrap())
            }
        }
        Value::String(v) => M::String(v.clone()),
        Value::Array(v) => M::List(v.iter().map(from_json).collect()),
        Value::Object(v) => M::Dict(
            v.iter()
                .map(|(k, v)| (M::String(k.clone()), from_json(v)))
                .collect(),
        ),
    }
}

pub fn to_json(value: &M) -> Result<Value, String> {
    Ok(match value {
        M::None => Value::Null,
        M::Bool(v) => json!(v),
        M::Int(v) => json!(v),
        M::BigInt(v) => serde_json::from_str(&v.to_string()).map_err(|_| "invalid JSON integer")?,
        M::Float(v) => serde_json::Number::from_f64(*v)
            .ok_or("non-finite float is not JSON")?
            .into(),
        M::String(v) | M::Path(v) => json!(v),
        M::Bytes(v) => json!(v),
        M::List(v) | M::Tuple(v) | M::Set(v) | M::FrozenSet(v) => {
            Value::Array(v.iter().map(to_json).collect::<Result<_, _>>()?)
        }
        M::Dict(v) => {
            let mut result = serde_json::Map::new();
            for (key, value) in v {
                let M::String(key) = key else {
                    return Err("JSON dictionary keys must be strings".into());
                };
                result.insert(key.clone(), to_json(value)?);
            }
            Value::Object(result)
        }
        M::NamedTuple {
            field_names,
            values,
            ..
        } => Value::Object(
            field_names
                .iter()
                .zip(values)
                .map(|(k, v)| Ok((k.clone(), to_json(v)?)))
                .collect::<Result<_, String>>()?,
        ),
        M::ClassInstance(v) if v.class_type.is_dataclass || v.class_type.name == "Response" => {
            to_json(&M::Dict(v.attrs.clone()))?
        }
        M::Date(v) => json!(format!("{:04}-{:02}-{:02}", v.year, v.month, v.day)),
        M::DateTime(v) => json!(format!(
            "{:04}-{:02}-{:02}T{}",
            v.year,
            v.month,
            v.day,
            iso_time(v.hour, v.minute, v.second, v.microsecond, v.offset_seconds)
        )),
        M::Time(v) => json!(iso_time(
            v.hour,
            v.minute,
            v.second,
            v.microsecond,
            v.offset_seconds
        )),
        M::TimeZone(v) => json!(v.name.clone().unwrap_or_else(|| offset(v.offset_seconds))),
        M::TimeDelta(v) => {
            json!(v.days as f64 * 86400.0 + v.seconds as f64 + v.microseconds as f64 / 1_000_000.0)
        }
        _ => return Err(format!("unsupported response type: {}", value.type_name())),
    })
}

/// Remote dataclasses are snapshots with eager fields, not live remote objects.
pub fn host_value(value: &mut M) {
    match value {
        M::ClassInstance(v) => {
            v.class_type.host_defined = true;
            v.attrs = std::mem::take(&mut v.attrs)
                .into_iter()
                .map(|(k, mut v)| {
                    host_value(&mut v);
                    (k, v)
                })
                .collect();
        }
        M::Dict(v) => {
            *v = std::mem::take(v)
                .into_iter()
                .map(|(k, mut v)| {
                    host_value(&mut v);
                    (k, v)
                })
                .collect();
        }
        M::NamedTuple { values, .. } => values.iter_mut().for_each(host_value),
        M::List(v) | M::Tuple(v) | M::Set(v) | M::FrozenSet(v) => v.iter_mut().for_each(host_value),
        _ => (),
    }
}

/// A completed HTTP value. Bodies stay bytes all the way to the server.
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub fn response(value: &M) -> Result<HttpResponse, String> {
    let (status, headers, body, text) = match value {
        M::None => (204, json!({}), Vec::new(), false),
        M::String(v) => (
            200,
            json!({"content-type":"text/plain; charset=utf-8"}),
            v.as_bytes().to_vec(),
            true,
        ),
        M::Bytes(v) => (
            200,
            json!({"content-type":"application/octet-stream"}),
            v.clone(),
            false,
        ),
        M::ClassInstance(v) if v.class_type.name == "Response" && !v.class_type.is_dataclass => {
            let fields = v
                .attrs
                .iter()
                .filter_map(|(k, v)| {
                    if let M::String(k) = k {
                        Some((k.as_str(), v))
                    } else {
                        None
                    }
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            let status = fields.get("status").ok_or("missing response status")?;
            let M::Int(status) = status else {
                return Err("response status must be int".into());
            };
            if !(200..=599).contains(status) {
                return Err("response status must be 200-599".into());
            }
            let body = fields.get("body").ok_or("missing response body")?;
            if !matches!(body, M::String(_) | M::Bytes(_)) {
                return Err("response body must be str or bytes".into());
            }
            let headers = to_json(fields.get("headers").ok_or("missing response headers")?)?;
            if !headers
                .as_object()
                .is_some_and(|h| h.values().all(Value::is_string))
            {
                return Err("response headers must be dict[str, str]".into());
            }
            (
                *status,
                headers,
                if [204, 205, 304].contains(status) {
                    Vec::new()
                } else {
                    match body {
                        M::String(s) => s.as_bytes().to_vec(),
                        M::Bytes(b) => b.clone(),
                        _ => unreachable!(),
                    }
                },
                ![204, 205, 304].contains(status) && matches!(body, M::String(_)),
            )
        }
        _ => (
            200,
            json!({"content-type":"application/json"}),
            to_json(value)?.to_string().into_bytes(),
            false,
        ),
    };
    let mut normalized = serde_json::Map::new();
    for (name, value) in headers.as_object().ok_or("invalid headers")? {
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
        {
            return Err("invalid response header name".into());
        }
        let value = value
            .as_str()
            .ok_or("response header value must be str")?
            .trim_matches(['\t', '\n', '\r', ' ']);
        if value
            .chars()
            .any(|c| matches!(c, '\0' | '\r' | '\n') || c as u32 > 255)
        {
            return Err("invalid response header value".into());
        }
        let name = name.to_ascii_lowercase();
        match normalized.get_mut(&name) {
            Some(Value::String(previous)) => {
                previous.push_str(", ");
                previous.push_str(value);
            }
            _ => {
                normalized.insert(name, json!(value));
            }
        }
    }
    if text && !normalized.contains_key("content-type") {
        normalized.insert("content-type".into(), json!("text/plain;charset=UTF-8"));
    }
    let body_size = body.len();
    let header_size: usize = normalized
        .iter()
        .map(|(k, v)| k.len() + v.as_str().unwrap().len())
        .sum();
    if body_size + header_size > 1024 * 1024 {
        return Err("result exceeds 1 MiB".into());
    }
    Ok(HttpResponse {
        status: status as u16,
        body,
        headers: normalized
            .into_iter()
            .map(|(k, v)| (k, v.as_str().unwrap().to_owned()))
            .collect(),
    })
}

fn offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let seconds = seconds.unsigned_abs();
    let mut result = format!("{sign}{:02}:{:02}", seconds / 3600, seconds / 60 % 60);
    if seconds % 60 != 0 {
        result.push_str(&format!(":{:02}", seconds % 60));
    }
    result
}
fn iso_time(hour: u8, minute: u8, second: u8, microsecond: u32, utc_offset: Option<i32>) -> String {
    let mut result = format!("{hour:02}:{minute:02}:{second:02}");
    if microsecond != 0 {
        result.push_str(&format!(".{microsecond:06}"));
    }
    if let Some(seconds) = utc_offset {
        result.push_str(&offset(seconds));
    }
    result
}

/// Native UTC clock/alarm values; Monty's datetime module need not implement
/// CPython's `fromtimestamp` constructor.
pub fn timestamp_reply(millis: i64) -> Result<Value, String> {
    use chrono::{Datelike, Timelike};
    let date = chrono::DateTime::from_timestamp_millis(millis).ok_or("invalid timestamp")?;
    let value = M::DateTime(monty_types::MontyDateTime {
        year: date.year(),
        month: date.month() as u8,
        day: date.day() as u8,
        hour: date.hour() as u8,
        minute: date.minute() as u8,
        second: date.second() as u8,
        microsecond: date.nanosecond() / 1000,
        offset_seconds: Some(0),
        timezone_name: None,
    });
    Ok(json!({"wire": serde_json::to_string(&value).map_err(|e| e.to_string())?}))
}
