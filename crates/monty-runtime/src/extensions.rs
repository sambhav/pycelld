//! Native and Python additions to explicitly named Python modules.
use crate::{Monty, PythonError, PythonValue, modules::SourceModule};
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

/// An importable Python module with Rust functions and Python helpers/classes.
/// Mount it with `Monty::with_module`; dotted paths create parent packages.
#[derive(Clone)]
pub struct PythonModule {
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) types: String,
    names: BTreeSet<String>,
    functions: BTreeMap<String, (usize, Arc<Callback>)>,
}
impl PythonModule {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            source: String::new(),
            types: String::new(),
            names: BTreeSet::new(),
            functions: BTreeMap::new(),
        }
    }
    /// Add source and matching public declarations to this module.
    pub fn with_python(mut self, source: &str, types: &str) -> celld_runtime::Result<Self> {
        let names = declarations(source)?;
        if names != declarations(types)? {
            return Err(
                "Python extension definitions and type declarations must export the same names"
                    .into(),
            );
        }
        self.validate(&names, source, types)?;
        self.append(source, types, names);
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
        let path: String = self.name.bytes().map(|b| format!("{b:02x}")).collect();
        let native_name = format!("_celld_extension_{path}_{name}");
        let Stmt::Expr(body) = &def.body[0] else {
            unreachable!()
        };
        let source = format!(
            "{}return {native_name}({})\n",
            &signature[..body.range.start().to_usize()],
            parameters.join(", ")
        );
        self.validate(&names, &source, signature)?;
        self.append(&source, signature, names);
        self.functions
            .insert(native_name, (parameters.len(), Arc::new(function)));
        Ok(self)
    }
}
impl PythonModule {
    fn validate(&self, names: &BTreeSet<String>, source: &str, types: &str) -> Result<(), String> {
        if names.is_empty() {
            return Err("an extension must define a public function or class".into());
        }
        for name in names {
            if name.starts_with('_')
                || (self.name == "celld" && BUILTINS.contains(&name.as_str()))
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
        self.types.push('\n');
        self.types.push_str(types);
        self.types.push('\n');
        self.names.extend(names);
    }
}

#[derive(Clone)]
pub(crate) struct Extensions {
    pub modules: BTreeMap<String, PythonModule>,
    pub functions: BTreeMap<String, (usize, Arc<Callback>)>,
}
impl Default for Extensions {
    fn default() -> Self {
        let mut celld = PythonModule::new("celld");
        celld.source = include_str!("context.py").into();
        celld.types = crate::TYPES.into();
        celld.names = BUILTINS.iter().map(|n| (*n).into()).collect();
        Self {
            modules: BTreeMap::from([("celld".into(), celld)]),
            functions: BTreeMap::new(),
        }
    }
}
impl Extensions {
    fn check_size(&self, module: &PythonModule) -> celld_runtime::Result<()> {
        let bytes: usize = self
            .modules
            .iter()
            .filter(|(name, _)| *name != &module.name)
            .map(|(_, m)| m.source.len() + m.types.len())
            .sum();
        if bytes + module.source.len() + module.types.len() > 2 * 1024 * 1024 {
            return Err("extensions exceed 2 MiB of source and declarations".into());
        }
        Ok(())
    }
    pub fn sources(&self) -> BTreeMap<String, SourceModule> {
        self.modules
            .iter()
            .map(|(name, module)| {
                (
                    name.clone(),
                    SourceModule {
                        source: module.source.clone(),
                        package: self
                            .modules
                            .keys()
                            .any(|n| n.starts_with(&format!("{name}."))),
                        worker: false,
                    },
                )
            })
            .collect()
    }
}
impl Monty {
    /// Mount a module at its declared import path. Register all modules before
    /// compiling workers; registration order does not constrain module imports.
    pub fn with_module(mut self, module: PythonModule) -> celld_runtime::Result<Self> {
        if !crate::modules::valid_module(&module.name)
            || (crate::package::reserved(&module.name) && !module.name.starts_with("celld."))
        {
            return Err(format!("invalid or reserved extension module: {}", module.name).into());
        }
        if self.extensions.modules.contains_key(&module.name) {
            return Err(format!("duplicate extension module: {}", module.name).into());
        }
        if self.extensions.modules.len() >= 256 {
            return Err("extensions exceed 256 modules".into());
        }
        self.extensions.check_size(&module)?;
        let extensions = Arc::make_mut(&mut self.extensions);
        extensions.functions.extend(module.functions.clone());
        extensions.modules.insert(module.name.clone(), module);
        Ok(self)
    }
    /// Add helpers to the built-in `celld` module. Use `with_module` to select
    /// a different import path.
    pub fn with_python(mut self, source: &str, types: &str) -> celld_runtime::Result<Self> {
        let module = self.extensions.modules["celld"]
            .clone()
            .with_python(source, types)?;
        self.extensions.check_size(&module)?;
        Arc::make_mut(&mut self.extensions)
            .modules
            .insert("celld".into(), module);
        Ok(self)
    }
    /// Add a native function to `celld`. PythonModule offers the same method
    /// for functions exported at other import paths.
    pub fn with_function(
        mut self,
        signature: &str,
        function: impl Fn(Vec<PythonValue>) -> PythonResult<PythonValue> + Send + Sync + 'static,
    ) -> celld_runtime::Result<Self> {
        let module = self.extensions.modules["celld"]
            .clone()
            .with_function(signature, function)?;
        self.extensions.check_size(&module)?;
        let extensions = Arc::make_mut(&mut self.extensions);
        extensions.functions.extend(module.functions.clone());
        extensions.modules.insert("celld".into(), module);
        Ok(self)
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
        if name.starts_with("_celld_") {
            return Err(format!("reserved extension name: {name}"));
        }
        if !name.starts_with('_') && !names.insert(name.to_owned()) {
            return Err(format!("duplicate extension name: {name}"));
        }
    }
    Ok(names)
}
