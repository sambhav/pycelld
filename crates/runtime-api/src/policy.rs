//! Trusted operator configuration. Resolving a worker never consults request
//! headers, Python values, principal claims, or worker variables.
use crate::{ApplicationIdentity, ExecutionLimits};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitOverrides {
    pub cpu_ms: Option<u64>,
    pub wall_ms: Option<u64>,
    pub max_operations: Option<usize>,
    pub max_payload_bytes: Option<usize>,
    pub max_recursion_depth: Option<usize>,
    pub max_memory_bytes: Option<usize>,
}
impl LimitOverrides {
    fn apply(&self, limits: &mut ExecutionLimits) {
        if let Some(v) = self.cpu_ms {
            limits.cpu_ms = v;
        }
        if let Some(v) = self.wall_ms {
            limits.wall_ms = v;
        }
        if let Some(v) = self.max_operations {
            limits.max_operations = v;
        }
        if let Some(v) = self.max_payload_bytes {
            limits.max_payload_bytes = v;
        }
        if let Some(v) = self.max_recursion_depth {
            limits.max_recursion_depth = v;
        }
        if let Some(v) = self.max_memory_bytes {
            limits.max_memory_bytes = Some(v);
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitRule {
    /// AND of exact matches: project_id, application_id, stage, tier, labels.NAME.
    pub matches: BTreeMap<String, String>,
    pub limits: LimitOverrides,
}
impl LimitRule {
    fn applies(&self, identity: &ApplicationIdentity) -> bool {
        self.matches.iter().all(|(key, expected)| {
            let actual = match key.as_str() {
                "project_id" => Some(identity.project_id.as_str()),
                "application_id" => Some(identity.application_id.as_str()),
                "stage" => identity.stage.as_deref(),
                "tier" => identity.tier.as_deref(),
                key => key
                    .strip_prefix("labels.")
                    .and_then(|k| identity.labels.get(k).map(String::as_str)),
            };
            actual == Some(expected.as_str())
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPolicy {
    pub revision: String,
    #[serde(default)]
    pub defaults: ExecutionLimits,
    /// Exact deployed worker ID -> operator identity. Multiple workers may map
    /// to one logical application. Unmapped workers are rejected.
    pub workers: BTreeMap<String, ApplicationIdentity>,
    /// Applied in order; later matching rules override only fields they set.
    #[serde(default)]
    pub rules: Vec<LimitRule>,
}

#[derive(Clone, Debug)]
pub struct ResolvedExecutionPolicy {
    pub revision: String,
    pub application: ApplicationIdentity,
    pub limits: ExecutionLimits,
}

fn identifier(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err("identifiers must contain 1-128 UTF-8 bytes without control characters".into());
    }
    Ok(())
}
impl ExecutionPolicy {
    pub fn validate(&self) -> Result<(), String> {
        identifier(&self.revision)?;
        self.defaults.validate()?;
        if self.workers.len() > 4096 || self.rules.len() > 256 {
            return Err("policy exceeds 4096 workers or 256 rules".into());
        }
        for rule in &self.rules {
            if rule.matches.is_empty() || rule.matches.len() > 20 {
                return Err("rule must have 1-20 exact-match selectors".into());
            }
            for (key, value) in &rule.matches {
                identifier(key)?;
                identifier(value)?;
                if !matches!(
                    key.as_str(),
                    "project_id" | "application_id" | "stage" | "tier"
                ) && !key.strip_prefix("labels.").is_some_and(|v| !v.is_empty())
                {
                    return Err(format!("unknown policy selector: {key}"));
                }
            }
            // Validate even unused rules, independently of cross-field cpu/wall
            // relationships which are checked on each fully resolved mapping.
            let mut test = self.defaults;
            rule.limits.apply(&mut test);
            test.wall_ms = test.wall_ms.max(test.cpu_ms);
            test.validate()?;
            if rule.limits.wall_ms == Some(0) {
                return Err("wall_ms must be positive".into());
            }
        }
        for (worker, identity) in &self.workers {
            identifier(worker)?;
            identifier(&identity.project_id)?;
            identifier(&identity.application_id)?;
            for value in [&identity.stage, &identity.tier].into_iter().flatten() {
                identifier(value)?;
            }
            if identity.labels.len() > 16 {
                return Err("at most 16 application labels".into());
            }
            for (key, value) in &identity.labels {
                identifier(key)?;
                identifier(value)?;
            }
            self.resolve(worker)?;
        }
        Ok(())
    }

    pub fn resolve(&self, worker: &str) -> Result<ResolvedExecutionPolicy, String> {
        let application = self
            .workers
            .get(worker)
            .ok_or("worker has no application mapping")?
            .clone();
        let mut limits = self.defaults;
        for rule in &self.rules {
            if rule.applies(&application) {
                rule.limits.apply(&mut limits);
            }
        }
        limits.validate()?;
        Ok(ResolvedExecutionPolicy {
            revision: self.revision.clone(),
            application,
            limits,
        })
    }
}

/// Atomically replace the configuration used by NEW invocations. Existing
/// invocations own their resolved snapshot and retain their original budgets.
/// Invalid explicit replacements are rejected without changing the current
/// state. A file controller can use `block` to fail admission on reload errors.
pub struct ExecutionPolicyStore(RwLock<Result<Arc<ExecutionPolicy>, String>>);
impl ExecutionPolicyStore {
    pub fn new(policy: ExecutionPolicy) -> Result<Self, String> {
        policy.validate()?;
        Ok(Self(RwLock::new(Ok(Arc::new(policy)))))
    }
    pub fn replace(&self, policy: ExecutionPolicy) -> Result<(), String> {
        policy.validate()?;
        *self.0.write().map_err(|_| "policy lock poisoned")? = Ok(Arc::new(policy));
        Ok(())
    }
    pub fn block(&self, reason: String) {
        *self.0.write().expect("policy lock poisoned") = Err(reason);
    }
    pub fn resolve(&self, worker: &str) -> Result<ResolvedExecutionPolicy, String> {
        let snapshot = self.0.read().map_err(|_| "policy lock poisoned")?.clone()?;
        snapshot.resolve(worker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> ExecutionPolicy {
        serde_json::from_value(serde_json::json!({
            "revision":"v1", "workers":{
                "one":{"project_id":"p1", "application_id":"api", "stage":"prod", "tier":"paid", "labels":{"region":"eu"}},
                "two":{"project_id":"p2", "application_id":"api", "stage":"test", "tier":"free"}
            }, "rules":[
                {"matches":{"tier":"paid"}, "limits":{"cpu_ms":500,"max_operations":50}},
                {"matches":{"project_id":"p1", "labels.region":"eu"}, "limits":{"max_operations":10}}
            ]
        })).unwrap()
    }
    #[test]
    fn mapping_and_order_do_not_mix_projects() {
        let p = policy();
        p.validate().unwrap();
        let one = p.resolve("one").unwrap();
        assert_eq!(one.limits.cpu_ms, 500);
        assert_eq!(one.limits.max_operations, 10);
        assert_eq!(p.resolve("two").unwrap().limits.cpu_ms, 100);
        assert!(p.resolve("unknown").is_err());
    }
    #[test]
    fn replacement_is_atomic_and_old_invocations_keep_their_limits() {
        let store = ExecutionPolicyStore::new(policy()).unwrap();
        let old = store.resolve("one").unwrap();
        let mut next = policy();
        next.revision = "v2".into();
        next.defaults.wall_ms = 1000;
        store.replace(next).unwrap();
        assert_eq!(old.limits.wall_ms, 30000);
        assert_eq!(store.resolve("one").unwrap().limits.wall_ms, 1000);
        let mut bad = policy();
        bad.defaults.max_memory_bytes = Some(1024);
        assert!(store.replace(bad).unwrap_err().contains("allocator"));
        assert_eq!(store.resolve("one").unwrap().revision, "v2");
        store.block("invalid file update".into());
        assert!(store.resolve("one").is_err());
        store.replace(policy()).unwrap();
        assert!(store.resolve("one").is_ok());
    }
    #[test]
    fn invalid_rules_and_effective_limits_are_rejected() {
        let mut p = policy();
        p.rules[0].matches.insert("tenant".into(), "p1".into());
        assert!(p.validate().is_err());
        let mut p = policy();
        p.defaults.wall_ms = 200;
        assert!(p.validate().unwrap_err().contains("cpu_ms"));
        let mut p = policy();
        p.rules[0].limits.max_recursion_depth = Some(101);
        assert!(p.validate().is_err());
    }

    #[test]
    fn concurrent_readers_never_mix_revisions_and_limits() {
        let store = Arc::new(ExecutionPolicyStore::new(policy()).unwrap());
        let writer = store.clone();
        let thread = std::thread::spawn(move || {
            for i in 0..500 {
                let mut p = policy();
                p.revision = i.to_string();
                p.defaults.wall_ms = 1000 + i;
                writer.replace(p).unwrap();
            }
        });
        for _ in 0..1000 {
            let snapshot = store.resolve("one").unwrap();
            if snapshot.revision != "v1" {
                assert_eq!(
                    snapshot.limits.wall_ms,
                    1000 + snapshot.revision.parse::<u64>().unwrap()
                );
            }
        }
        thread.join().unwrap();
    }

    #[test]
    fn documented_policy_resolves_production_and_preview() {
        let p: ExecutionPolicy = serde_json::from_str(include_str!(
            "../../../examples/execution-policy/policy.json"
        ))
        .unwrap();
        p.validate().unwrap();
        assert_eq!(p.resolve("orders-api-prod").unwrap().limits.cpu_ms, 500);
        assert_eq!(
            p.resolve("orders-api-preview")
                .unwrap()
                .limits
                .max_operations,
            1000
        );
    }
}
