use monty::{MontyRun, RunProgress};
use monty_types::{CompileOptions, MontyObject, PrintWriter, ResourceLimits, ResourceTracker};
use ruff_python_ast::{Expr, Stmt};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[derive(Clone, Debug)]
enum Kind {
    Any,
    Str,
    Int,
    Float,
    Bool,
    None,
    List(Box<Kind>),
    Dict(Box<Kind>),
    Union(Vec<Kind>),
}
impl Kind {
    fn parse(expr: Option<&Expr>) -> Result<Self, String> {
        let Some(expr) = expr else {
            return Ok(Self::Any);
        };
        Ok(match expr {
            Expr::Name(n) => match n.id.as_str() {
                "Any" | "Json" => Self::Any,
                "str" => Self::Str,
                "int" => Self::Int,
                "float" => Self::Float,
                "bool" => Self::Bool,
                "list" => Self::List(Box::new(Self::Any)),
                "dict" => Self::Dict(Box::new(Self::Any)),
                _ => return Err(format!("unsupported annotation {}", n.id)),
            },
            Expr::NoneLiteral(_) => Self::None,
            Expr::BinOp(b) if b.op == ruff_python_ast::Operator::BitOr => Self::Union(vec![
                Self::parse(Some(&b.left))?,
                Self::parse(Some(&b.right))?,
            ]),
            Expr::Subscript(s) => match s.value.as_ref() {
                Expr::Name(n) if n.id.as_str() == "list" => {
                    Self::List(Box::new(Self::parse(Some(&s.slice))?))
                }
                Expr::Name(n) if n.id.as_str() == "dict" => {
                    let Expr::Tuple(t) = s.slice.as_ref() else {
                        return Err("use dict[str, T]".into());
                    };
                    if t.elts.len() != 2 || !matches!(Self::parse(t.elts.first())?, Self::Str) {
                        return Err("JSON dictionary keys must be str".into());
                    }
                    Self::Dict(Box::new(Self::parse(t.elts.get(1))?))
                }
                _ => return Err("unsupported container annotation".into()),
            },
            _ => {
                return Err(
                    "unsupported annotation; Monty supports JSON types and T | None".into(),
                );
            }
        })
    }
    fn valid(&self, v: &Value) -> bool {
        match self {
            Self::Any => true,
            Self::Str => v.is_string(),
            Self::Int => v.is_number() && !v.to_string().contains(['.', 'e', 'E']),
            Self::Float => v.is_number(),
            Self::Bool => v.is_boolean(),
            Self::None => v.is_null(),
            Self::List(k) => v.as_array().is_some_and(|v| v.iter().all(|v| k.valid(v))),
            Self::Dict(k) => v
                .as_object()
                .is_some_and(|v| v.values().all(|v| k.valid(v))),
            Self::Union(ks) => ks.iter().any(|k| k.valid(v)),
        }
    }
}

#[derive(Clone)]
struct Parameter {
    kind: Kind,
    required: bool,
}
#[derive(Clone)]
pub struct Function {
    runner: MontyRun,
    pub(crate) sources: Arc<crate::observability::SourceMap>,
    pub(crate) rpc: bool,
    parameters: BTreeMap<String, Parameter>,
    pub(crate) extensions: Arc<crate::extensions::Extensions>,
}
impl Function {
    pub(crate) fn start_with_limits(
        &self,
        args: &Value,
        context: &Value,
        body: &[u8],
        limits: celld_runtime::ExecutionLimits,
        output: &mut crate::observability::Output,
    ) -> Result<RunProgress, crate::Failure> {
        limits.validate()?;
        let args = args
            .as_object()
            .ok_or_else(|| crate::Failure::arguments("arguments must be an object"))?;
        for (name, p) in &self.parameters {
            match args.get(name) {
                Some(v) if !p.kind.valid(v) => {
                    return Err(crate::Failure::arguments(format!(
                        "invalid type for {name}"
                    )));
                }
                None if p.required => {
                    return Err(crate::Failure::arguments(format!(
                        "missing argument {name}"
                    )));
                }
                _ => (),
            }
        }
        for name in args.keys() {
            if !self.parameters.contains_key(name) {
                return Err(crate::Failure::arguments(format!(
                    "unknown argument {name}"
                )));
            }
        }
        let limits = ResourceLimits {
            max_duration: Some(Duration::from_millis(limits.cpu_ms)),
            max_recursion_depth: 100,
            max_suspensions: limits.max_operations,
            ..Default::default()
        };
        let mut inputs = vec![
            crate::value::from_json(&Value::Object(args.clone())),
            crate::value::from_json(context),
            MontyObject::Bytes(body.to_vec()),
            MontyObject::Function {
                name: "_celld_host".into(),
                docstring: None,
            },
        ];
        inputs.extend(
            self.extensions
                .functions
                .keys()
                .map(|name| MontyObject::Function {
                    name: name.clone(),
                    docstring: None,
                }),
        );
        self.runner
            .clone()
            .start(
                inputs,
                ResourceTracker::new(limits),
                PrintWriter::Callback(output),
            )
            .map_err(|error| output.failure(error))
    }
}

#[derive(Clone)]
pub struct Module {
    functions: BTreeMap<String, Function>,
    http: Option<Function>,
}
pub(crate) struct Compilation {
    pub graph: crate::modules::Graph,
    pub entry: String,
    // Stable durable identity, defining module, class name.
    pub classes: Vec<(String, String, String)>,
}
pub(crate) fn prepare(
    source: &str,
    extensions: &crate::extensions::Extensions,
) -> Result<Compilation, String> {
    let package = crate::package::Package::parse(source)?;
    let mut modules = extensions.sources();
    let mut classes = Vec::new();
    let mut source_lines = BTreeMap::new();
    for (name, mut module) in package.modules {
        if modules.contains_key(&name) {
            return Err(format!("worker module conflicts with host module: {name}"));
        }
        source_lines.insert(name.clone(), module.source.lines().count());
        let names = durable_classes(&module.source)?;
        let mut identities = BTreeMap::new();
        for class in names {
            let id = if package.single && name == package.entry {
                class.clone()
            } else {
                format!("{name}.{class}")
            };
            classes.push((id.clone(), name.clone(), class.clone()));
            identities.insert(class, id);
        }
        module
            .source
            .push_str(&durable_proxies(&module.source, &identities)?);
        modules.insert(name, module);
    }
    let mut graph = crate::modules::Graph::new(modules)?;
    graph.source_lines = source_lines;
    // Validate all explicit package interfaces before any worker is started.
    for (name, module) in &graph.modules {
        if module.worker {
            crate::modules::exports(name, module)?;
        }
    }
    Ok(Compilation {
        graph,
        entry: package.entry,
        classes,
    })
}
impl Module {
    #[cfg(test)]
    pub fn compile(source: &str) -> Result<Self, String> {
        let extensions = Arc::new(crate::extensions::Extensions::default());
        let compiled = prepare(source, &extensions)?;
        Self::compile_graph(&compiled.graph, &compiled.entry, None, extensions)
    }
    #[cfg(test)]
    pub fn compile_class(source: &str, class: &str) -> Result<Self, String> {
        let extensions = Arc::new(crate::extensions::Extensions::default());
        let compiled = prepare(source, &extensions)?;
        Self::compile_graph(
            &compiled.graph,
            &compiled.entry,
            Some((&compiled.entry, class)),
            extensions,
        )
    }
    pub(crate) fn compile_graph(
        graph: &crate::modules::Graph,
        entry: &str,
        class: Option<(&str, &str)>,
        extensions: Arc<crate::extensions::Extensions>,
    ) -> Result<Self, String> {
        let mut selected = Vec::new();
        if let Some((module, class_name)) = class {
            let parsed = ruff_python_parser::parse_module(&graph.modules[module].source)
                .map_err(|e| e.to_string())?;
            let class_def = parsed
                .syntax()
                .body
                .iter()
                .find_map(|s| match s {
                    Stmt::ClassDef(c) if c.name.as_str() == class_name => Some(c),
                    _ => None,
                })
                .ok_or("unknown durable class")?;
            let definitions: BTreeMap<_, _> = class_def
                .body
                .iter()
                .filter_map(|s| match s {
                    Stmt::FunctionDef(f) => Some((f.name.to_string(), f)),
                    _ => None,
                })
                .collect();
            if let Some(init) = definitions.get("__init__") {
                let names: Vec<_> = init
                    .parameters
                    .args
                    .iter()
                    .chain(init.parameters.kwonlyargs.iter())
                    .map(|p| p.parameter.name.as_str())
                    .collect();
                if names != ["self", "id", "ctx"]
                    || !init.parameters.kwonlyargs.is_empty()
                    || init.parameters.args.iter().any(|p| p.default.is_some())
                    || !init.parameters.posonlyargs.is_empty()
                    || init.parameters.vararg.is_some()
                    || init.parameters.kwarg.is_some()
                {
                    return Err("durable constructors must take self, id: str, ctx: Context".into());
                }
            } else {
                return Err("durable classes need __init__(self, id: str, ctx: Context)".into());
            }

            for (name, f) in definitions {
                if !name.starts_with('_') {
                    selected.push((name, module.to_owned(), f.clone()));
                }
            }
        } else {
            for name in crate::modules::exports(entry, &graph.modules[entry])? {
                if let Some((module, original)) = graph.resolve(entry, &name)? {
                    let parsed = ruff_python_parser::parse_module(&graph.modules[&module].source)
                        .map_err(|e| e.to_string())?;
                    let functions: Vec<_> = parsed
                        .syntax()
                        .body
                        .iter()
                        .filter_map(|s| match s {
                            Stmt::FunctionDef(f) if f.name == original => Some(f),
                            _ => None,
                        })
                        .collect();
                    if functions.len() > 1 {
                        return Err(format!("duplicate function {original}"));
                    }
                    if let Some(f) = functions.first() {
                        selected.push((name, module, (*f).clone()));
                    }
                }
            }
        }
        let (prefix, sources) = graph.render_mapped(entry)?;
        let sources = Arc::new(sources);
        let mut functions = BTreeMap::new();
        let mut http = None;
        for (name, module, f) in selected {
            let is_http = class.is_none()
                && f.decorator_list.len() == 1
                && graph.is_http_decorator(&module, &f.decorator_list[0].expression)?;
            if is_http && http.is_some() {
                return Err("export exactly one @http handler".into());
            }
            if !f.parameters.posonlyargs.is_empty()
                || f.parameters.vararg.is_some()
                || f.parameters.kwarg.is_some()
            {
                return Err(format!(
                    "{name}: use named parameters, not positional-only or variadic parameters"
                ));
            }
            let mut parameters = BTreeMap::new();
            let mut context_parameter = false;
            for p in f
                .parameters
                .args
                .iter()
                .chain(f.parameters.kwonlyargs.iter())
            {
                let pname = p.parameter.name.to_string();
                if class.is_some() && pname == "self" {
                    continue;
                }
                if is_http && pname == "request" {
                    if p.default.is_some() {
                        return Err("@http request cannot have a default".into());
                    }
                    continue;
                }
                if pname == "ctx" {
                    if class.is_some() {
                        return Err(format!(
                            "{name}: store the constructor's ctx on self instead of declaring a method ctx parameter"
                        ));
                    }
                    context_parameter = true;
                    continue;
                }
                if pname.starts_with("_celld_") {
                    return Err("_celld_ parameter names are reserved".into());
                }
                let kind = Kind::parse(p.parameter.annotation.as_deref())
                    .map_err(|e| format!("{name}.{pname}: {e}"))?;
                parameters.insert(
                    pname,
                    Parameter {
                        kind,
                        required: p.default.is_none(),
                    },
                );
            }
            // Only a name parsed from the source is inserted into code. Request arguments
            // are input data, never interpolation/eval. Private functions cannot be selected.
            if class.is_some()
                && f.parameters.args.first().map(|p| p.parameter.name.as_str()) != Some("self")
            {
                return Err(format!("{name}: class methods must take self"));
            }
            if !f.decorator_list.is_empty() && !is_http {
                return Err(format!(
                    "{name}: method/function decorators are not supported in Monty"
                ));
            }

            if is_http
                && (!parameters.is_empty()
                    || !f
                        .parameters
                        .args
                        .iter()
                        .chain(f.parameters.kwonlyargs.iter())
                        .any(|p| p.parameter.name.as_str() == "request"))
            {
                return Err(
                    "@http handlers take request: Request and optionally ctx: Context".into(),
                );
            }
            let target = class.map_or_else(
                || graph.target(&module, &f.name),
                |_| format!("_celld_instance.{name}"),
            );
            let call = format!(
                "{}{}({}{}**_celld_args)",
                if f.is_async { "await " } else { "" },
                target,
                if is_http {
                    "request=_celld_context.request, "
                } else {
                    ""
                },
                if context_parameter {
                    "ctx=_celld_context, "
                } else {
                    ""
                }
            );
            let construction = class.map_or_else(String::new, |(module, class)| format!("_celld_import({module:?})\n_celld_instance = {}(id=_celld_context.id, ctx=_celld_context)\n_celld_instance.id = _celld_context.id", graph.target(module, &format!("_celld_impl_{class}"))));
            let context = if context_parameter || class.is_some() || is_http {
                "_celld_context = _celld_import('celld').Context(_celld_metadata)\n_celld_context.request.body = _celld_request_body"
            } else {
                ""
            };
            let code = format!("{prefix}\n{context}\n{construction}\n{call}");
            let runner = MontyRun::new(
                code,
                "app.py",
                vec![
                    "_celld_args".into(),
                    "_celld_metadata".into(),
                    "_celld_request_body".into(),
                    "_celld_host".into(),
                ]
                .into_iter()
                .chain(extensions.functions.keys().cloned())
                .collect(),
                CompileOptions::default(),
            )
            .map_err(|e| e.to_string())?;
            let function = Function {
                runner,
                sources: sources.clone(),
                rpc: class.is_some(),
                parameters,
                extensions: extensions.clone(),
            };
            if is_http {
                http = Some(function);
            } else {
                functions.insert(name, function);
            }
        }
        Ok(Self { functions, http })
    }
    pub(crate) fn http(&self) -> Option<&Function> {
        self.http.as_ref()
    }
    pub fn get(&self, name: &str) -> Option<&Function> {
        self.functions.get(name)
    }
}

/// A durable class is an ordinary class whose constructor declares `ctx`.
/// Dataclasses and local helper/iterator classes are left untouched.
pub fn durable_classes(source: &str) -> Result<Vec<String>, String> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
    Ok(parsed.syntax().body.iter().filter_map(|stmt| {
        let Stmt::ClassDef(class) = stmt else { return None };
        if class.name.starts_with('_') { return None; }
        class.body.iter().any(|stmt| {
            matches!(stmt, Stmt::FunctionDef(f) if f.name.as_str() == "__init__" &&
                f.parameters.args.iter().chain(f.parameters.kwonlyargs.iter()).any(|p| p.parameter.name.as_str() == "ctx") &&
                f.parameters.args.iter().chain(f.parameters.kwonlyargs.iter()).any(|p| p.parameter.name.as_str() == "id"))
        }).then(|| class.name.to_string())
    }).collect())
}

fn durable_proxies(source: &str, identities: &BTreeMap<String, String>) -> Result<String, String> {
    let names = durable_classes(source)?;
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
    let mut output = String::new();
    for stmt in &parsed.syntax().body {
        let Stmt::ClassDef(class) = stmt else {
            continue;
        };
        let name = class.name.as_str();
        if !names.iter().any(|n| n == name) {
            continue;
        }
        let identity = &identities[name];
        output.push_str(&format!("\n_celld_impl_{name} = {name}\nclass {name}:\n    def __init__(self, id: str, ctx: Context):\n        if not isinstance(id, str) or not id or len(id) > 1024:\n            raise ValueError('object id must contain 1-1024 characters')\n        self.id = id\n"));
        for stmt in &class.body {
            let Stmt::FunctionDef(f) = stmt else { continue };
            let method = f.name.as_str();
            if method.starts_with('_') || method == "alarm" {
                continue;
            }
            if !f.parameters.posonlyargs.is_empty()
                || f.parameters.vararg.is_some()
                || f.parameters.kwarg.is_some()
                || !f.decorator_list.is_empty()
            {
                return Err(format!(
                    "{name}.{method}: use ordinary methods with named parameters"
                ));
            }
            let mut parameters = vec!["self".to_string()];
            let mut values = Vec::new();
            for (keyword_only, params) in [
                (false, &f.parameters.args),
                (true, &f.parameters.kwonlyargs),
            ] {
                if keyword_only && !params.is_empty() {
                    parameters.push("*".into());
                }
                for p in params {
                    let key = p.parameter.name.as_str();
                    if key == "self" || key == "ctx" {
                        continue;
                    }
                    parameters.push(
                        source[p.range.start().to_usize()..p.range.end().to_usize()].to_string(),
                    );
                    values.push(format!("{key:?}: {key}"));
                }
            }
            output.push_str(&format!("    {}def {method}({}):\n        return _celld_host('object.call', {identity:?}, self.id, {method:?}, {{{}}})\n", if f.is_async { "async " } else { "" }, parameters.join(", "), values.join(", ")));
        }
    }
    Ok(output)
}
