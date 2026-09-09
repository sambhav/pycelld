fn main() -> anyhow::Result<()> {
    let public_errors = pycelld::celld::env_vars::flag("CELLD_PYTHON_PUBLIC_ERRORS", false)?;
    pycelld::run(pycelld::Monty::new().with_public_errors(public_errors))
}
