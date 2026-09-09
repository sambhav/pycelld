//! The patched celld host and its extensible native Python runtime.
//!
//! Configure [`Monty`] with Python functions/classes and native Rust functions,
//! then call [`run`] from your binary's main function. Cargo includes the host
//! patches through this crate's dependencies; no consumer-side patch step is needed.

pub use celld;
pub use celld_monty::{
    ExcType, FetchContext, FetchDecision, FetchRequest, Monty, PythonError, PythonModule,
    PythonNetworkPolicy, PythonResult, PythonValue,
};
pub use celld_runtime as runtime;

// Keep upstream's allocator and pressure accounting in custom binaries too.
// Applications which supply their own allocator can disable default features.
#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Register a runtime and run celld's command line, including `dev`, `deploy`,
/// `types`, and the production server. Call once, from the process entry point.
pub fn run(runtime: impl runtime::Runtime + 'static) -> anyhow::Result<()> {
    celld::native::register(runtime)?;
    celld::command::run()
}
