fn main() -> anyhow::Result<()> {
    let public_errors = pycelld::celld::env_vars::flag("CELLD_PYTHON_PUBLIC_ERRORS", false)?;
    let mut runtime = pycelld::Monty::new().with_public_errors(public_errors);
    if let Some(path) = std::env::var_os("CELLD_PYTHON_NETWORK_POLICY") {
        let policy = pycelld::PythonNetworkPolicy::load(path)
            .map_err(|error| anyhow::anyhow!("invalid Python network policy: {error}"))?;
        runtime = runtime.with_network_policy(policy);
    }
    pycelld::run(runtime)
}
