use pycelld::{ExcType, Monty, PythonError, PythonModule, PythonValue};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let native = PythonModule::new("acme.native").with_function(
        "def shout(text: str, *, excited: bool = True) -> str: ...",
        |args| match args.as_slice() {
            [PythonValue::String(text), PythonValue::Bool(excited)] => Ok(PythonValue::String(
                format!("{}{}", text.to_uppercase(), if *excited { "!" } else { "" }),
            )),
            _ => Err(PythonError::new(
                ExcType::TypeError,
                Some("expected text and excited".into()),
            )),
        },
    )?;
    let helpers = PythonModule::new("acme.greeters")
        .with_python(include_str!("helpers.py"), include_str!("helpers.pyi"))?;
    let runtime = Monty::new().with_module(helpers)?.with_module(native)?;
    pycelld::run(runtime)?;
    Ok(())
}
