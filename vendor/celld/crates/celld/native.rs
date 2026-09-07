//! Register one statically linked native runtime before starting celld.
//! The ordinary JavaScript worker path remains available alongside it.
pub use celld_runtime::*;
use std::{path::Path, sync::OnceLock};

static RUNTIME: OnceLock<Box<dyn Runtime>> = OnceLock::new();

pub fn register(runtime: impl Runtime + 'static) -> anyhow::Result<()> {
    RUNTIME
        .set(Box::new(runtime))
        .map_err(|_| anyhow::anyhow!("native runtime already registered"))
}
pub fn runtime() -> Option<&'static dyn Runtime> {
    RUNTIME.get().map(Box::as_ref)
}
pub(crate) fn is_entry(entry: &str) -> bool {
    runtime().is_some_and(|r| {
        Path::new(entry)
            .extension()
            .is_some_and(|e| e == r.descriptor().extension)
    })
}
pub(crate) fn is_artifact(source: &str) -> bool {
    runtime().is_some_and(|r| source.starts_with(r.descriptor().artifact_prefix))
}
pub(crate) fn compile(source: &str) -> anyhow::Result<Box<dyn Program>> {
    let runtime = runtime().ok_or_else(|| anyhow::anyhow!("native runtime is not registered"))?;
    Ok(runtime.compile(source)?)
}
pub(crate) fn bundle(root: &Path, entry: &str) -> anyhow::Result<Vec<u8>> {
    let source = std::fs::read_to_string(root.join(entry))?;
    compile(&source)?;
    Ok(format!(
        "{}{}",
        runtime().unwrap().descriptor().artifact_prefix,
        source
    )
    .into_bytes())
}
