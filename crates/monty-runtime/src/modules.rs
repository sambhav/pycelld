//! Link declared Python modules into isolated namespaces inside Monty.
//! Parsing and binding resolution are Rust-only. Imports never read host files
//! at execution time, and each module is initialized once per invocation.
use ruff_python_ast::{
    self as ast, Expr, ExprContext, Stmt,
    visitor::{self, Visitor},
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Default)]
pub(crate) struct SourceModule {
    pub source: String,
    pub package: bool,
    pub worker: bool,
}

#[derive(Clone)]
pub(crate) enum Binding {
    Local,
    From(String, String),
    Module,
}

pub(crate) fn valid_module(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !part.starts_with("_celld_")
        })
        && ruff_python_parser::parse_module(&format!("import {name}")).is_ok()
}

pub(crate) fn absolute(
    module: &str,
    package: bool,
    from: Option<&str>,
    level: u32,
) -> Result<String> {
    if level == 0 {
        return Ok(from.unwrap_or("").to_owned());
    }
    let mut parts: Vec<_> = module.split('.').collect();
    if !package {
        parts.pop();
    }
    if level as usize > parts.len() {
        return Err(format!("{module}: relative import escapes its package"));
    }
    for _ in 1..level {
        parts.pop();
    }
    if let Some(from) = from {
        parts.extend(from.split('.'));
    }
    Ok(parts.join("."))
}

pub(crate) fn bindings(name: &str, module: &SourceModule) -> Result<BTreeMap<String, Binding>> {
    let parsed =
        ruff_python_parser::parse_module(&module.source).map_err(|e| format!("{name}: {e}"))?;
    let mut found = BTreeMap::new();
    let scope = Scope::body(&parsed.syntax().body, false);
    for name in scope.locals {
        found.insert(name, Binding::Local);
    }
    for statement in &parsed.syntax().body {
        match statement {
            Stmt::Import(i) => {
                for alias in &i.names {
                    found.insert(
                        alias.asname.as_ref().map_or_else(
                            || alias.name.split('.').next().unwrap().to_owned(),
                            ToString::to_string,
                        ),
                        Binding::Module,
                    );
                }
            }
            Stmt::ImportFrom(i) => {
                let target = absolute(name, module.package, i.module.as_deref(), i.level)?;
                for alias in &i.names {
                    if alias.name.as_str() == "*" {
                        return Err(format!("{name}: use named imports instead of import *"));
                    }
                    found.insert(
                        alias.asname.as_ref().unwrap_or(&alias.name).to_string(),
                        Binding::From(target.clone(), alias.name.to_string()),
                    );
                }
            }
            _ => {
                for name in Scope::body(std::slice::from_ref(statement), false).locals {
                    found.insert(name, Binding::Local);
                }
            }
        }
    }
    Ok(found)
}

pub(crate) fn exports(name: &str, module: &SourceModule) -> Result<BTreeSet<String>> {
    let parsed = ruff_python_parser::parse_module(&module.source).map_err(|e| e.to_string())?;
    let mut explicit = None;
    struct Uses(usize);
    impl<'a> Visitor<'a> for Uses {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if matches!(expr, Expr::Name(n) if n.id == "__all__") {
                self.0 += 1;
            }
            visitor::walk_expr(self, expr);
        }
    }
    let mut uses = Uses(0);
    uses.visit_body(&parsed.syntax().body);
    for statement in &parsed.syntax().body {
        if let Stmt::Assign(a) = statement {
            if a.targets
                .iter()
                .any(|t| matches!(t, Expr::Name(n) if n.id == "__all__"))
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
                for value in values {
                    let Expr::StringLiteral(s) = value else {
                        return Err("__all__ entries must be literal names".into());
                    };
                    let name = s.value.to_string();
                    if name.starts_with('_') || !names.insert(name) {
                        return Err("__all__ contains private or duplicate names".into());
                    }
                }
                explicit = Some(names);
            }
        }
    }
    if uses.0 != usize::from(explicit.is_some()) {
        return Err("__all__ must be one plain literal assignment".into());
    }
    let bindings = bindings(name, module)?;
    if let Some(names) = explicit {
        for name in &names {
            if !bindings.contains_key(name) {
                return Err(format!("unknown __all__ export: {name}"));
            }
        }
        Ok(names)
    } else {
        Ok(bindings
            .into_keys()
            .filter(|n| !n.starts_with('_'))
            .collect())
    }
}

pub(crate) struct Graph {
    pub modules: BTreeMap<String, SourceModule>,
    indices: BTreeMap<String, usize>,
    pub source_lines: BTreeMap<String, usize>,
}
impl Graph {
    pub fn new(mut modules: BTreeMap<String, SourceModule>) -> Result<Self> {
        for name in modules.keys().cloned().collect::<Vec<_>>() {
            if !valid_module(&name) {
                return Err(format!("invalid Python module path: {name}"));
            }
            let mut parent = name.as_str();
            while let Some((prefix, _)) = parent.rsplit_once('.') {
                if let Some(existing) = modules.get(prefix) {
                    if !existing.package {
                        return Err(format!("{prefix} is a module, not a package"));
                    }
                } else {
                    modules.insert(
                        prefix.into(),
                        SourceModule {
                            package: true,
                            ..Default::default()
                        },
                    );
                }
                parent = prefix;
            }
        }
        let indices = modules
            .keys()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();
        Ok(Self {
            modules,
            indices,
            source_lines: BTreeMap::new(),
        })
    }
    /// Only the imported celld marker is special; similarly named user decorators are not.
    pub fn is_http_decorator(&self, module: &str, expr: &Expr) -> Result<bool> {
        let symbols = bindings(module, &self.modules[module])?;
        Ok(match expr {
            Expr::Name(n) => {
                matches!(symbols.get(n.id.as_str()), Some(Binding::From(m, n)) if m == "celld" && n == "http")
            }
            Expr::Attribute(a) if a.attr.as_str() == "http" => {
                if let Expr::Name(n) = a.value.as_ref() {
                    let parsed = ruff_python_parser::parse_module(&self.modules[module].source)
                        .map_err(|e| e.to_string())?;
                    matches!(symbols.get(n.id.as_str()), Some(Binding::Module)) && parsed.syntax().body.iter().any(|s| matches!(s, Stmt::Import(i) if i.names.iter().any(|a| a.name.as_str() == "celld" && a.asname.as_ref().map_or("celld", |n| n.as_str()) == n.id.as_str())))
                } else {
                    false
                }
            }
            _ => false,
        })
    }
    pub fn target(&self, module: &str, name: &str) -> String {
        format!("_celld_m{}.{}", self.indices[module], name)
    }
    pub fn resolve(&self, module: &str, name: &str) -> Result<Option<(String, String)>> {
        let mut module = module.to_owned();
        let mut name = name.to_owned();
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert((module.clone(), name.clone())) {
                return Err(format!("cyclic export: {module}.{name}"));
            }
            let Some(source) = self.modules.get(&module) else {
                return Ok(None);
            };
            if !source.worker {
                return Ok(None);
            }
            match bindings(&module, source)?.get(&name) {
                Some(Binding::Local) => return Ok(Some((module, name))),
                Some(Binding::From(m, n)) => {
                    module = m.clone();
                    name = n.clone();
                }
                _ => return Ok(None),
            }
        }
    }
    pub fn render_mapped(&self, entry: &str) -> Result<(String, crate::observability::SourceMap)> {
        let mut sources = crate::observability::SourceMap::default();
        let mut output = String::from(include_str!("module_loader.py"));
        for (name, index) in &self.indices {
            output.push_str(&format!("\n_celld_m{index} = _CelldModule()\n_celld_m{index}.__name__ = {name:?}\n_celld_m{index}.__package__ = {:?}\n", if self.modules[name].package { name.as_str() } else { name.rsplit_once('.').map_or("", |p| p.0) }));
        }
        for (name, module) in &self.modules {
            let index = self.indices[name];
            let parsed = ruff_python_parser::parse_module(&module.source)
                .map_err(|e| format!("{name}: {e}"))?;
            let mut globals = Scope::body(&parsed.syntax().body, false).locals;
            globals.extend(["__name__".into(), "__package__".into()]);
            struct Globals(BTreeSet<String>);
            impl<'a> Visitor<'a> for Globals {
                fn visit_stmt(&mut self, stmt: &'a Stmt) {
                    if let Stmt::Global(g) = stmt {
                        self.0.extend(g.names.iter().map(ToString::to_string));
                    }
                    visitor::walk_stmt(self, stmt);
                }
            }
            let mut declared = Globals(BTreeSet::new());
            declared.visit_body(&parsed.syntax().body);
            globals.extend(declared.0);
            let mut rewrite = Rewrite {
                graph: self,
                module: name,
                source: &module.source,
                globals,
                scopes: vec![],
                edits: vec![],
                error: None,
            };
            rewrite.visit_body(&parsed.syntax().body);
            if let Some(error) = rewrite.error {
                return Err(format!("{name}: {error}"));
            }
            let mut code = module.source.clone();
            // Byte positions carry their original line through every linker edit.
            let mut line = 1usize;
            let mut origins: Vec<usize> = code
                .bytes()
                .map(|b| {
                    let current = line;
                    if b == b'\n' {
                        line += 1;
                    }
                    current
                })
                .collect();
            rewrite.edits.sort_by_key(|(start, end, _)| (*start, *end));
            for (start, end, replacement) in rewrite.edits.into_iter().rev() {
                let original = origins.get(start).copied().unwrap_or(line);
                origins.splice(start..end, std::iter::repeat_n(original, replacement.len()));
                code.replace_range(start..end, &replacement);
            }
            output.push_str(&format!("\ndef _celld_init_{index}():\n"));
            if code.trim().is_empty() {
                output.push_str("    pass\n");
            } else {
                let mut offset = 0;
                let mut generated_line = output.bytes().filter(|b| *b == b'\n').count() + 1;
                let original_lines: Vec<_> = module.source.lines().collect();
                for line in code.lines() {
                    let original = origins.get(offset).copied().unwrap_or(1);
                    if module.worker
                        && original <= self.source_lines.get(name).copied().unwrap_or(usize::MAX)
                    {
                        let filename = if name == "__worker__" {
                            "app.py".into()
                        } else {
                            format!(
                                "{}{}",
                                name.replace('.', "/"),
                                if module.package {
                                    "/__init__.py"
                                } else {
                                    ".py"
                                }
                            )
                        };
                        let preview = original_lines
                            .get(original - 1)
                            .copied()
                            .unwrap_or("")
                            .to_owned();
                        sources
                            .0
                            .push((generated_line, filename, original, preview));
                    }
                    offset += line.len() + 1;
                    generated_line += 1;
                    output.push_str("    ");
                    output.push_str(line);
                    output.push('\n');
                }
            }
        }
        output.push_str("\n_celld_modules = {\n");
        for (name, index) in &self.indices {
            output.push_str(&format!(
                "    {name:?}: (_celld_m{index}, _celld_init_{index}),\n"
            ));
        }
        output.push_str(&format!("}}\n_celld_import({entry:?})\n"));
        Ok((output, sources))
    }
}

#[derive(Default)]
struct Scope {
    locals: BTreeSet<String>,
    globals: BTreeSet<String>,
    nonlocals: BTreeSet<String>,
    class: bool,
}
impl Scope {
    fn body(body: &[Stmt], class: bool) -> Self {
        let mut scope = Self {
            class,
            ..Default::default()
        };
        scope.visit_body(body);
        for name in scope.globals.union(&scope.nonlocals) {
            scope.locals.remove(name);
        }
        scope
    }
    fn parameters(&mut self, parameters: &ast::Parameters) {
        self.locals
            .extend(parameters.iter().map(|p| p.name().to_string()));
    }
}
impl<'a> Visitor<'a> for Scope {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(f) => {
                self.locals.insert(f.name.to_string());
            }
            Stmt::ClassDef(c) => {
                self.locals.insert(c.name.to_string());
            }
            Stmt::Import(i) => {
                for a in &i.names {
                    self.locals.insert(a.asname.as_ref().map_or_else(
                        || a.name.split('.').next().unwrap().to_owned(),
                        ToString::to_string,
                    ));
                }
            }
            Stmt::ImportFrom(i) => {
                for a in &i.names {
                    self.locals
                        .insert(a.asname.as_ref().unwrap_or(&a.name).to_string());
                }
            }
            Stmt::Global(g) => self.globals.extend(g.names.iter().map(ToString::to_string)),
            Stmt::Nonlocal(g) => self
                .nonlocals
                .extend(g.names.iter().map(ToString::to_string)),
            _ => visitor::walk_stmt(self, stmt),
        }
    }
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(n) if n.ctx != ExprContext::Load => {
                self.locals.insert(n.id.to_string());
            }
            Expr::Lambda(_) => {}
            // Comprehensions own their iteration variables, not their walrus targets.
            Expr::ListComp(c) => {
                self.visit_expr(&c.elt);
                for g in &c.generators {
                    self.visit_expr(&g.iter);
                    for e in &g.ifs {
                        self.visit_expr(e);
                    }
                }
            }
            Expr::SetComp(c) => {
                self.visit_expr(&c.elt);
                for g in &c.generators {
                    self.visit_expr(&g.iter);
                    for e in &g.ifs {
                        self.visit_expr(e);
                    }
                }
            }
            Expr::DictComp(c) => {
                if let Some(key) = &c.key {
                    self.visit_expr(key);
                }
                self.visit_expr(&c.value);
                for g in &c.generators {
                    self.visit_expr(&g.iter);
                    for e in &g.ifs {
                        self.visit_expr(e);
                    }
                }
            }
            Expr::Generator(c) => {
                self.visit_expr(&c.elt);
                for g in &c.generators {
                    self.visit_expr(&g.iter);
                    for e in &g.ifs {
                        self.visit_expr(e);
                    }
                }
            }
            _ => visitor::walk_expr(self, expr),
        }
    }
    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.locals.insert(name.to_string());
        }
        visitor::walk_except_handler(self, handler);
    }
}

struct Rewrite<'a> {
    graph: &'a Graph,
    module: &'a str,
    source: &'a str,
    globals: BTreeSet<String>,
    scopes: Vec<Scope>,
    edits: Vec<(usize, usize, String)>,
    error: Option<String>,
}
impl Rewrite<'_> {
    fn global(&self, name: &str, store: bool) -> bool {
        for (i, scope) in self.scopes.iter().enumerate().rev() {
            if scope.class {
                if i + 1 == self.scopes.len() && (store || scope.locals.contains(name)) {
                    return false;
                }
                continue;
            }
            if scope.globals.contains(name) {
                return true;
            }
            if scope.locals.contains(name) {
                return false;
            }
        }
        self.globals.contains(name)
    }
    fn name(&self, name: &str, store: bool) -> String {
        if self.global(name, store) {
            self.graph.target(self.module, name)
        } else {
            name.into()
        }
    }
    fn replace(&mut self, start: usize, end: usize, text: String) {
        self.edits.push((start, end, text));
    }
    fn publish(&mut self, name: &str, start: usize, end: usize) {
        if self.global(name, true) {
            let line = self.source[..start].rfind('\n').map_or(0, |p| p + 1);
            let indent: String = self.source[line..start]
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();
            self.replace(
                end,
                end,
                format!(
                    "\n{indent}{} = {name}",
                    self.graph.target(self.module, name)
                ),
            );
        }
    }
    fn import(&mut self, import: &ast::StmtImportFrom) -> Result<String> {
        let target = absolute(
            self.module,
            self.graph.modules[self.module].package,
            import.module.as_deref(),
            import.level,
        )?;
        if target == "__future__" {
            return if import
                .names
                .iter()
                .all(|a| a.name.as_str() == "annotations" && a.asname.is_none())
            {
                Ok("pass".into())
            } else {
                Err("only future annotations is supported in Monty modules".into())
            };
        }
        if import.names.iter().any(|a| a.name.as_str() == "*") {
            return Err("use named imports instead of import *".into());
        }
        let custom = self.graph.modules.contains_key(&target);
        let mut statements = Vec::new();
        for alias in &import.names {
            let local = alias.asname.as_ref().unwrap_or(&alias.name).as_str();
            let binding = self.name(local, true);
            if custom {
                let symbols = bindings(&target, &self.graph.modules[&target])?;
                // celld's aliases are annotations rather than runtime bindings.
                if target == "celld" && ["Json", "SqlValue"].contains(&alias.name.as_str()) {
                    statements.push(format!("{binding} = object"));
                } else if symbols.contains_key(alias.name.as_str())
                    || self
                        .graph
                        .modules
                        .contains_key(&format!("{target}.{}", alias.name))
                {
                    statements.push(format!(
                        "{binding} = _celld_from({target:?}, {:?})",
                        alias.name.as_str()
                    ));
                } else {
                    return Err(format!("cannot import {} from {target}", alias.name));
                }
            } else {
                statements.push(format!("from {target} import {} as {local}", alias.name));
                if binding != local {
                    statements.push(format!("{binding} = {local}"));
                }
            }
        }
        Ok(statements.join("; "))
    }
    fn comprehension<'a>(&mut self, generators: &'a [ast::Comprehension], values: &[&'a Expr]) {
        let mut scope = Scope::default();
        for generator in generators {
            scope.visit_expr(&generator.target);
        }
        // The first iterable is evaluated in the surrounding scope.
        if let Some(first) = generators.first() {
            self.visit_expr(&first.iter);
        }
        self.scopes.push(scope);
        for (i, generator) in generators.iter().enumerate() {
            self.visit_expr(&generator.target);
            if i > 0 {
                self.visit_expr(&generator.iter);
            }
            for condition in &generator.ifs {
                self.visit_expr(condition);
            }
        }
        for value in values {
            self.visit_expr(value);
        }
        self.scopes.pop();
    }
}
impl<'a> Visitor<'a> for Rewrite<'_> {
    fn visit_body(&mut self, body: &'a [Stmt]) {
        for stmt in body {
            self.visit_stmt(stmt);
            if self.scopes.last().is_some_and(|s| s.class) {
                let names = Scope::body(std::slice::from_ref(stmt), true).locals;
                self.scopes.last_mut().unwrap().locals.extend(names);
            }
        }
    }
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(f) => {
                for d in &f.decorator_list {
                    match self.graph.is_http_decorator(self.module, &d.expression) {
                        Ok(true) => self.replace(
                            d.range.start().to_usize(),
                            d.range.end().to_usize(),
                            String::new(),
                        ),
                        Ok(false) => self.visit_decorator(d),
                        Err(error) => self.error = Some(error),
                    }
                }
                self.visit_parameters(&f.parameters);
                if let Some(r) = &f.returns {
                    self.visit_annotation(r);
                }
                let mut scope = Scope::body(&f.body, false);
                scope.parameters(&f.parameters);
                self.scopes.push(scope);
                self.visit_body(&f.body);
                self.scopes.pop();
                self.publish(
                    &f.name,
                    f.range.start().to_usize(),
                    f.range.end().to_usize(),
                );
            }
            Stmt::ClassDef(c) => {
                for d in &c.decorator_list {
                    self.visit_decorator(d);
                }
                if let Some(a) = &c.arguments {
                    self.visit_arguments(a);
                }
                self.scopes.push(Scope {
                    class: true,
                    ..Default::default()
                });
                self.visit_body(&c.body);
                self.scopes.pop();
                self.publish(
                    &c.name,
                    c.range.start().to_usize(),
                    c.range.end().to_usize(),
                );
            }
            Stmt::Global(g) => self.replace(
                g.range.start().to_usize(),
                g.range.end().to_usize(),
                "pass".into(),
            ),
            Stmt::ImportFrom(i) => match self.import(i) {
                Ok(code) => {
                    self.replace(i.range.start().to_usize(), i.range.end().to_usize(), code)
                }
                Err(error) => self.error = Some(error),
            },
            Stmt::Import(i) => {
                let mut code = Vec::new();
                for a in &i.names {
                    let local = a
                        .asname
                        .as_ref()
                        .map_or_else(|| a.name.split('.').next().unwrap(), |n| n.as_str());
                    let binding = self.name(local, true);
                    if self.graph.modules.contains_key(a.name.as_str()) {
                        code.push(format!("_celld_import({:?})", a.name.as_str()));
                        code.push(format!(
                            "{binding} = _celld_import({:?})",
                            if a.asname.is_some() {
                                a.name.as_str()
                            } else {
                                local
                            }
                        ));
                    } else {
                        code.push(format!(
                            "import {}{}",
                            a.name,
                            a.asname
                                .as_ref()
                                .map_or(String::new(), |n| format!(" as {n}"))
                        ));
                        if binding != local {
                            code.push(format!("{binding} = {local}"));
                        }
                    }
                }
                self.replace(
                    i.range.start().to_usize(),
                    i.range.end().to_usize(),
                    code.join("; "),
                );
            }
            _ => visitor::walk_stmt(self, stmt),
        }
    }
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(n) => {
                let name = self.name(&n.id, n.ctx != ExprContext::Load);
                if name != n.id.as_str() {
                    self.replace(n.range.start().to_usize(), n.range.end().to_usize(), name);
                }
            }
            Expr::Lambda(l) => {
                if let Some(p) = &l.parameters {
                    self.visit_parameters(p);
                }
                let mut scope = Scope::default();
                if let Some(p) = &l.parameters {
                    scope.parameters(p);
                }
                self.scopes.push(scope);
                self.visit_expr(&l.body);
                self.scopes.pop();
            }
            Expr::ListComp(c) => self.comprehension(&c.generators, &[&c.elt]),
            Expr::SetComp(c) => self.comprehension(&c.generators, &[&c.elt]),
            Expr::DictComp(c) => {
                let mut values = vec![c.value.as_ref()];
                if let Some(key) = &c.key {
                    values.push(key.as_ref());
                }
                self.comprehension(&c.generators, &values);
            }
            Expr::Generator(c) => self.comprehension(&c.generators, &[&c.elt]),
            Expr::Named(n) if matches!(n.target.as_ref(), Expr::Name(t) if self.global(&t.id, true)) =>
            {
                self.error = Some("module/global assignment expressions are unsupported; use an assignment statement".into());
            }
            _ => visitor::walk_expr(self, expr),
        }
    }
    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(h) = handler;
        if h.name.as_ref().is_some_and(|n| self.global(n, true)) {
            self.error = Some("module/global exception aliases are unsupported; catch the exception inside a function".into());
        }
        visitor::walk_except_handler(self, handler);
    }
}
