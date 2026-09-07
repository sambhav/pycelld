fn main() -> anyhow::Result<()> {
    pycelld::run(pycelld::Monty::new())
}
