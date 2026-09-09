use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Host-assigned execution identity. Runtime instances and invocation IDs are
/// ephemeral; durable object identity is carried separately by Invocation.object.
/// Empty metadata is useful to embedders/tests but is never issued by celld.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ExecutionMetadata {
    #[serde(default)]
    pub application: ApplicationIdentity,
    #[serde(default)]
    pub policy_revision: String,
    pub worker_id: String,
    pub deployment_id: String,
    pub invocation_id: String,
    pub runtime_instance_id: String,
    pub root_invocation_id: String,
    pub parent_invocation_id: Option<String>,
    pub principal: Option<VerifiedPrincipal>,
}

/// Operator-owned placement, independent of the authenticated caller and storage keys.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationIdentity {
    pub project_id: String,
    pub application_id: String,
    pub stage: Option<String>,
    pub tier: Option<String>,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
}

/// An embedding host may attach a principal after authentication. Neither the
/// supplied binary nor the interpreter authenticates headers or worker values.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VerifiedPrincipal {
    pub tenant_id: Option<String>,
    pub subject: String,
    pub claims: Value,
}

/// Host ceilings, copied into Python for inspection only. Interpreter CPU means
/// active elapsed interpreter time, excluding suspension; it is not an OS quota.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionLimits {
    pub cpu_ms: u64,
    pub wall_ms: u64,
    pub max_operations: usize,
    pub max_payload_bytes: usize,
    pub max_recursion_depth: usize,
    /// Reserved for an invocation-safe allocator/isolation backend. A configured
    /// value is rejected, never silently ignored by the shared-process runtime.
    pub max_memory_bytes: Option<usize>,
}
impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            cpu_ms: 100,
            wall_ms: 30_000,
            max_operations: 10_000,
            max_payload_bytes: 1024 * 1024,
            max_recursion_depth: 100,
            max_memory_bytes: None,
        }
    }
}
impl ExecutionLimits {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_memory_bytes.is_some() {
            return Err("max_memory_bytes is unavailable: Monty's process-wide allocator cannot isolate concurrent invocations".into());
        }
        if !(1..=100).contains(&self.max_recursion_depth) {
            return Err("max_recursion_depth must be 1-100".into());
        }
        if !(1..=60_000).contains(&self.cpu_ms) {
            return Err("cpu_ms must be 1-60000".into());
        }
        if !(1..=300_000).contains(&self.wall_ms) {
            return Err("wall_ms must be 1-300000".into());
        }
        if self.cpu_ms > self.wall_ms {
            return Err("cpu_ms must not exceed wall_ms".into());
        }
        if !(1..=1_000_000).contains(&self.max_operations) {
            return Err("max_operations must be 1-1000000".into());
        }
        // Existing native value conversion and filesystem bounds cap payloads at 1 MiB.
        if !(1..=1024 * 1024).contains(&self.max_payload_bytes) {
            return Err("max_payload_bytes must be 1-1048576".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_are_validated_at_boundaries() {
        let defaults = ExecutionLimits::default();
        assert!(defaults.validate().is_ok());
        for limits in [
            ExecutionLimits {
                cpu_ms: 0,
                ..defaults
            },
            ExecutionLimits {
                cpu_ms: 60_001,
                ..defaults
            },
            ExecutionLimits {
                wall_ms: 0,
                ..defaults
            },
            ExecutionLimits {
                wall_ms: 300_001,
                ..defaults
            },
            ExecutionLimits {
                wall_ms: 99,
                ..defaults
            },
            ExecutionLimits {
                max_operations: 0,
                ..defaults
            },
            ExecutionLimits {
                max_operations: 1_000_001,
                ..defaults
            },
            ExecutionLimits {
                max_payload_bytes: 0,
                ..defaults
            },
            ExecutionLimits {
                max_payload_bytes: 1_048_577,
                ..defaults
            },
        ] {
            assert!(limits.validate().is_err(), "{limits:?}");
        }
        assert!(serde_json::from_str::<ExecutionLimits>(r#"{"unknown":1}"#).is_err());
        assert!(serde_json::from_str::<ExecutionLimits>(r#"{"cpu_ms":-1}"#).is_err());
    }
}
