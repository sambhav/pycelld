//! Build-time additions to the typed `celld` Python module.
use crate::{Monty, PythonError, PythonValue};
use ruff_python_ast::{Expr, Stmt};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub type PythonResult<T> = Result<T, PythonError>;
type Callback = dyn Fn(Vec<PythonValue>) -> PythonResult<PythonValue> + Send + Sync;

pub(crate) const BUILTINS: &[&str] = &[
    "Context", "Storage", "Alarms", "Request", "Response", "Json", "SqlValue",
];

#[derive(Clone, Default)]
pub(crate) struct Extensions {
    pub source: String,
    pub types: String,
    pub names: BTreeSet<String>,
    pub functions: BTreeMap<String, (usize, Arc<Callback>)>,
}

impl Monty {
    /// Add Python helpers and classes, with declarations appended to `celld types`.
    /// Only functions declared in the worker entry file become HTTP handlers.
    pub fn with_python(mut self, source: &str, types: &str) -> celld_runtime::Result<Self> {
        let names = declarations(source)?;
        if names != declarations(types)? {
            return Err(
                "Python extension definitions and type declarations must export the same names"
                    .into(),
            );
        }
        self.extensions.validate(&names, source, types)?;
        let allowed = self.extensions.names.union(&names).cloned().collect();
        let source = crate::exports::celld_imports(source, &allowed)?;
        let types = crate::exports::celld_imports(types, &allowed)?;
        Arc::make_mut(&mut self.extensions).append(&source, &types, names);
        Ok(self)
    }

    /// Add a bounded, synchronous Rust function using a typed Python signature.
    /// Python binds defaults and keywords; Rust receives values in parameter order.
    /// Bytes and class values cross directly, without JSON conversion.
    pub fn with_function(
        mut self,
        signature: &str,
        function: impl Fn(Vec<PythonValue>) -> PythonResult<PythonValue> + Send + Sync + 'static,
    ) -> celld_runtime::Result<Self> {
        let parsed = ruff_python_parser::parse_module(signature).map_err(|e| e.to_string())?;
        let [Stmt::FunctionDef(def)] = parsed.syntax().body.as_slice() else {
            return Err("provide one typed Python function signature ending in ...".into());
        };
        if def.is_async
            || !def.decorator_list.is_empty()
            || def.type_params.is_some()
            || def.parameters.vararg.is_some()
            || def.parameters.kwarg.is_some()
            || def.returns.is_none()
            || !matches!(def.body.as_slice(), [Stmt::Expr(e)] if matches!(e.value.as_ref(), Expr::EllipsisLiteral(_)))
        {
            return Err("use def name(typed parameters) -> ReturnType: ...; async, decorators and variadic parameters are unsupported".into());
        }
        let parameters = def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
            .map(|p| {
                if p.parameter.annotation.is_none() {
                    return Err("every injected function parameter must have a type annotation");
                }
                if p.parameter.name.starts_with("_celld_") {
                    return Err("_celld_ parameter names are reserved");
                }
                Ok(p.parameter.name.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let name = def.name.to_string();
        let names = BTreeSet::from([name.clone()]);
        let native_name = format!("_celld_extension_{name}");
        let Stmt::Expr(body) = &def.body[0] else {
            unreachable!()
        };
        let source = format!(
            "{}return {native_name}({})\n",
            &signature[..body.range.start().to_usize()],
            parameters.join(", ")
        );
        self.extensions.validate(&names, &source, signature)?;
        let extensions = Arc::make_mut(&mut self.extensions);
        extensions.append(&source, signature, names);
        extensions
            .functions
            .insert(native_name, (parameters.len(), Arc::new(function)));
        Ok(self)
    }
}

impl Extensions {
    fn validate(&self, names: &BTreeSet<String>, source: &str, types: &str) -> Result<(), String> {
        if names.is_empty() {
            return Err("an extension must define a public function or class".into());
        }
        for name in names {
            if name.starts_with('_')
                || BUILTINS.contains(&name.as_str())
                || self.names.contains(name)
            {
                return Err(format!("duplicate or reserved extension name: {name}"));
            }
        }
        if self.source.len() + source.len() > 256 * 1024
            || self.types.len() + types.len() > 256 * 1024
        {
            return Err("Python extensions exceed 256 KiB".into());
        }
        Ok(())
    }

    fn append(&mut self, source: &str, types: &str, names: BTreeSet<String>) {
        self.source.push('\n');
        self.source.push_str(source);
        self.source.push('\n');
        if self.types.is_empty() {
            self.types.push_str(crate::TYPES);
        }
        self.types.push('\n');
        self.types.push_str(types);
        self.types.push('\n');
        self.names.extend(names);
    }
}

// Imports and private helpers are implementation details. Public declarations
// are explicit so an import cannot accidentally become part of the host API.
fn declarations(source: &str) -> Result<BTreeSet<String>, String> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
    let mut names = BTreeSet::new();
    for statement in &parsed.syntax().body {
        let name = match statement {
            Stmt::FunctionDef(def) => def.name.as_str(),
            Stmt::ClassDef(def) => def.name.as_str(),
            Stmt::Import(_) | Stmt::ImportFrom(_) => continue,
            Stmt::Expr(e) if matches!(e.value.as_ref(), Expr::StringLiteral(_)) => continue,
            _ => return Err("extension modules contain imports, functions and classes".into()),
        };
        if name.starts_with("_celld_") || BUILTINS.contains(&name) {
            return Err(format!("reserved extension name: {name}"));
        }
        if !name.starts_with('_') && !names.insert(name.to_owned()) {
            return Err(format!("duplicate extension name: {name}"));
        }
    }
    Ok(names)
}
