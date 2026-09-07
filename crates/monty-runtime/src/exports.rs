use monty::{MontyRun, RunProgress};
use monty_types::{CompileOptions, MontyObject, PrintWriter, ResourceLimits, ResourceTracker};
use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{Expr, Stmt};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

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
    pub(crate) rpc: bool,
    parameters: BTreeMap<String, Parameter>,
}
impl Function {
    pub(crate) fn start(
        &self,
        args: &Value,
        context: &Value,
    ) -> Result<RunProgress, crate::Failure> {
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
            max_duration: Some(Duration::from_millis(100)),
            max_recursion_depth: 100,
            ..Default::default()
        };
        self.runner
            .clone()
            .start(
                vec![
                    crate::value::from_json(&Value::Object(args.clone())),
                    crate::value::from_json(context),
                    MontyObject::Function {
                        name: "_celld_host".into(),
                        docstring: None,
                    },
                ],
                ResourceTracker::new(limits),
                PrintWriter::Disabled,
            )
            .map_err(crate::Failure::python)
    }
}

#[derive(Clone)]
pub struct Module {
    functions: BTreeMap<String, Function>,
}
impl Module {
    /// Discovers only direct, public function declarations in the entry module.
    /// A literal __all__ optionally restricts that set; imports/classes/aliases are excluded.
    pub fn compile(source: &str) -> Result<Self, String> {
        Self::compile_inner(source, None)
    }
    pub fn compile_class(source: &str, class: &str) -> Result<Self, String> {
        Self::compile_inner(source, Some(class))
    }
    fn compile_inner(source: &str, class: Option<&str>) -> Result<Self, String> {
        let original = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
        let mut code = source.as_bytes().to_vec();
        for statement in &original.syntax().body {
            if let Stmt::ImportFrom(import) = statement {
                if import
                    .module
                    .as_ref()
                    .is_some_and(|name| name.as_str() == "celld")
                    && import.level == 0
                {
                    for alias in &import.names {
                        if !matches!(
                            alias.name.as_str(),
                            "Context"
                                | "Storage"
                                | "Alarms"
                                | "Request"
                                | "Response"
                                | "Json"
                                | "SqlValue"
                        ) || alias.asname.is_some()
                        {
                            return Err("use unaliased imports from celld: Context, Storage, Alarms, Request, Response, Json, SqlValue".into());
                        }
                    }
                    for byte in
                        &mut code[import.range.start().to_usize()..import.range.end().to_usize()]
                    {
                        if *byte != b'\n' && *byte != b'\r' {
                            *byte = b' ';
                        }
                    }
                }
            }
        }
        let source = std::str::from_utf8(&code).map_err(|e| e.to_string())?;
        let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
        let mut definitions = BTreeMap::new();
        let mut explicit = None;
        let class_def = if let Some(name) = class {
            Some(
                parsed
                    .syntax()
                    .body
                    .iter()
                    .find_map(|stmt| match stmt {
                        Stmt::ClassDef(c) if c.name.as_str() == name => Some(c),
                        _ => None,
                    })
                    .ok_or_else(|| format!("unknown Python class {name}"))?,
            )
        } else {
            None
        };
        let statements = class_def.map_or(&parsed.syntax().body, |c| &c.body);
        for stmt in statements {
            match stmt {
                Stmt::FunctionDef(f) => {
                    if definitions.insert(f.name.to_string(), f).is_some() {
                        return Err(format!("duplicate function {}", f.name));
                    }
                }
                Stmt::Assign(a)
                    if a.targets
                        .iter()
                        .any(|t| matches!(t,Expr::Name(n) if n.id.as_str()=="__all__")) =>
                {
                    if explicit.is_some() || a.targets.len() != 1 {
                        return Err("__all__ must be assigned once".into());
                    }
                    let values = match a.value.as_ref() {
                        Expr::List(l) => &l.elts,
                        Expr::Tuple(t) => &t.elts,
                        _ => return Err("__all__ must be a literal list or tuple".into()),
                    };
                    let mut names = BTreeSet::new();
                    for v in values {
                        let Expr::StringLiteral(s) = v else {
                            return Err("__all__ entries must be literal names".into());
                        };
                        let name = s.value.to_string();
                        if !names.insert(name) {
                            return Err("duplicate __all__ entry".into());
                        }
                    }
                    explicit = Some(names);
                }
                _ => (),
            }
        }
        // Dynamic export changes must fail rather than silently expose a larger API.
        struct ExportUses(usize);
        impl<'a> Visitor<'a> for ExportUses {
            fn visit_expr(&mut self, expr: &'a Expr) {
                if matches!(expr, Expr::Name(n) if n.id.as_str()=="__all__") {
                    self.0 += 1;
                }
                visitor::walk_expr(self, expr);
            }
        }
        let mut uses = ExportUses(0);
        for stmt in statements {
            uses.visit_stmt(stmt);
        }
        if uses.0 != usize::from(explicit.is_some()) {
            return Err("__all__ must be one plain literal assignment; dynamic or annotated exports are unsupported".into());
        }
        let construct_with_context = class.is_some();
        if let Some(init) = definitions.get("__init__").filter(|_| class.is_some()) {
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
        } else if class.is_some() {
            return Err("durable classes need __init__(self, id: str, ctx: Context)".into());
        }
        let proxies = durable_proxies(source)?;
        let names = explicit.unwrap_or_else(|| {
            definitions
                .keys()
                .filter(|n| !n.starts_with('_'))
                .cloned()
                .collect()
        });
        let mut functions = BTreeMap::new();
        for name in names {
            if name.starts_with('_') {
                return Err("private names cannot be exported".into());
            }
            let f = definitions
                .get(&name)
                .ok_or_else(|| format!("{name} is not a top-level function"))?;
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
            if !f.decorator_list.is_empty() {
                return Err(format!(
                    "{name}: method/function decorators are not supported in Monty"
                ));
            }
            let target = class.map_or_else(|| name.clone(), |_| format!("_celld_instance.{name}"));
            let call = format!(
                "{}{}({}**_celld_args)",
                if f.is_async { "await " } else { "" },
                target,
                if context_parameter {
                    "ctx=_celld_context, "
                } else {
                    ""
                }
            );
            let construction = class.map_or_else(String::new, |class| format!("_celld_instance = _celld_impl_{class}(id=_celld_context.id, ctx=_celld_context)\n_celld_instance.id = _celld_context.id"));
            let code = format!(
                "{}\n{source}\n{proxies}\n{context_init}\n{construction}\n{call}",
                include_str!("context.py"),
                context_init = if context_parameter || construct_with_context {
                    "_celld_context = Context(_celld_metadata)"
                } else {
                    ""
                }
            );
            let runner = MontyRun::new(
                code,
                "app.py",
                vec![
                    "_celld_args".into(),
                    "_celld_metadata".into(),
                    "_celld_host".into(),
                ],
                CompileOptions::default(),
            )
            .map_err(|e| e.to_string())?;
            functions.insert(
                name,
                Function {
                    runner,
                    rpc: class.is_some(),
                    parameters,
                },
            );
        }
        Ok(Self { functions })
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

fn durable_proxies(source: &str) -> Result<String, String> {
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
            output.push_str(&format!("    {}def {method}({}):\n        return _celld_host('object.call', {name:?}, self.id, {method:?}, {{{}}})\n", if f.is_async { "async " } else { "" }, parameters.join(", "), values.join(", ")));
        }
    }
    Ok(output)
}
