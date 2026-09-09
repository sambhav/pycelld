//! Process-owned live configuration. File I/O and parsing happen off worker
//! threads; an invocation resolves one atomic validated snapshot at admission.
use celld_runtime::{
    ApplicationIdentity, ExecutionLimits,
    policy::{ExecutionPolicy, ExecutionPolicyStore, ResolvedExecutionPolicy},
};
use std::{
    io::Read,
    path::Path,
    sync::{Arc, OnceLock},
    time::Duration,
};

enum Source {
    Legacy(ExecutionLimits),
    Live(Arc<ExecutionPolicyStore>),
}
static POLICY: OnceLock<Result<Source, String>> = OnceLock::new();

fn read_policy(path: &Path) -> Result<Vec<u8>, String> {
    const MAX: u64 = 1024 * 1024;
    let metadata = std::fs::metadata(path).map_err(|e| format!("execution policy: {e}"))?;
    if !metadata.is_file() || metadata.len() > MAX {
        return Err("execution policy must be a regular file of at most 1 MiB".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("execution policy: {e}"))?;
    if bytes.len() > MAX as usize {
        return Err("execution policy exceeds 1 MiB".into());
    }
    Ok(bytes)
}
fn parse(bytes: &[u8]) -> Result<ExecutionPolicy, String> {
    let policy: ExecutionPolicy =
        serde_json::from_slice(bytes).map_err(|e| format!("execution policy: {e}"))?;
    policy.validate()?;
    Ok(policy)
}
fn initialize() -> Result<Source, String> {
    let Some(path) = std::env::var_os("CELLD_EXECUTION_POLICY") else {
        return crate::env_vars::native_limits()
            .map(Source::Legacy)
            .map_err(|e| e.to_string());
    };
    if std::env::var_os("CELLD_NATIVE_LIMITS").is_some() {
        return Err("use CELLD_EXECUTION_POLICY or CELLD_NATIVE_LIMITS, not both".into());
    }
    let path = std::path::PathBuf::from(path);
    let mut previous = read_policy(&path)?;
    let store = Arc::new(ExecutionPolicyStore::new(parse(&previous)?)?);
    let live = store.clone();
    std::thread::Builder::new().name("execution-policy".into()).spawn(move || {
        let mut last_error: Option<String> = None;
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let update = read_policy(&path).and_then(|bytes| {
                if bytes == previous && last_error.is_none() { return Ok(()); }
                let policy = parse(&bytes)?;
                let revision = policy.revision.clone();
                live.replace(policy)?;
                previous = bytes;
                tracing::info!(target: "native", %revision, "execution policy updated");
                Ok(())
            });
            match update {
                Ok(()) => last_error = None,
                Err(error) => {
                    if last_error.as_ref() != Some(&error) {
                        tracing::error!(target: "native", %error, "execution policy reload failed; new invocations blocked");
                    }
                    live.block(error.clone());
                    last_error = Some(error);
                }
            }
        }
    }).map_err(|e| e.to_string())?;
    Ok(Source::Live(store))
}

/// Initialize/validate once. File-backed policy updates continue in the
/// background without reloading compiled workers or resetting durable state.
pub fn execution_policy() -> anyhow::Result<()> {
    source().map(|_| ())
}
fn source() -> anyhow::Result<&'static Source> {
    POLICY
        .get_or_init(initialize)
        .as_ref()
        .map_err(|e| anyhow::anyhow!(e.clone()))
}

/// Embedders can install an operator policy before startup, or replace an
/// existing programmatic policy. Do not mix programmatic updates with the file
/// controller: the next file change would supersede a programmatic update.
pub fn replace_execution_policy(policy: ExecutionPolicy) -> anyhow::Result<()> {
    policy.validate().map_err(anyhow::Error::msg)?;
    if POLICY.get().is_none() {
        let store = ExecutionPolicyStore::new(policy.clone()).map_err(anyhow::Error::msg)?;
        if POLICY.set(Ok(Source::Live(Arc::new(store)))).is_ok() {
            return Ok(());
        }
    }
    match source()? {
        Source::Live(store) => store.replace(policy).map_err(anyhow::Error::msg),
        Source::Legacy(_) => anyhow::bail!(
            "install a programmatic policy before starting celld, or configure CELLD_EXECUTION_POLICY"
        ),
    }
}

pub(crate) fn resolve_execution_policy(worker: &str) -> anyhow::Result<ResolvedExecutionPolicy> {
    match source()? {
        Source::Live(store) => store.resolve(worker).map_err(anyhow::Error::msg),
        Source::Legacy(limits) => Ok(ResolvedExecutionPolicy {
            revision: "legacy".into(),
            application: ApplicationIdentity {
                project_id: "default".into(),
                application_id: worker.into(),
                ..Default::default()
            },
            limits: *limits,
        }),
    }
}
