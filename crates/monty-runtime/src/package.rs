//! A bounded, deterministic source artifact. Only declared project Python files
//! are read during deployment; the executing worker needs no filesystem loader.
use crate::modules::{SourceModule, absolute, valid_module};
use ruff_python_ast::{
    Stmt,
    visitor::{self, Visitor},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const PREFIX: &str = "# celld:python-package-v1\n";
const MAX_BYTES: usize = 1024 * 1024;
const MAX_FILES: usize = 256;

pub(crate) struct Package {
    pub entry: String,
    pub modules: BTreeMap<String, SourceModule>,
    pub single: bool,
}
impl Package {
    pub fn parse(source: &str) -> Result<Self, String> {
        let Some(source) = source.strip_prefix(PREFIX) else {
            if source.len() > 256 * 1024 {
                return Err("Monty source exceeds 256 KiB".into());
            }
            return Ok(Self {
                entry: "__worker__".into(),
                modules: BTreeMap::from([(
                    "__worker__".into(),
                    SourceModule {
                        source: source.into(),
                        worker: true,
                        package: false,
                    },
                )]),
                single: true,
            });
        };
        if source.len() > MAX_BYTES * 6 {
            return Err("Python package artifact exceeds its size limit".into());
        }
        let value: serde_json::Value =
            serde_json::from_str(source).map_err(|e| format!("invalid Python package: {e}"))?;
        let entry = value["entry"]
            .as_str()
            .ok_or("package entry must be a module name")?
            .to_owned();
        let files = value["modules"]
            .as_object()
            .ok_or("package modules must be an object")?;
        let mut modules = BTreeMap::new();
        for (name, item) in files {
            modules.insert(
                name.clone(),
                SourceModule {
                    source: item["source"]
                        .as_str()
                        .ok_or("module source must be a string")?
                        .into(),
                    package: item["package"]
                        .as_bool()
                        .ok_or("module package flag must be a bool")?,
                    worker: true,
                },
            );
        }
        let result = Self {
            entry,
            modules,
            single: value["single"].as_bool().unwrap_or(false),
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<(), String> {
        if !self.modules.contains_key(&self.entry) {
            return Err("Python package entry module is missing".into());
        }
        if self.modules.len() > MAX_FILES
            || self.modules.values().map(|m| m.source.len()).sum::<usize>() > MAX_BYTES
        {
            return Err("Python packages are limited to 256 modules and 1 MiB of source".into());
        }
        for (name, module) in &self.modules {
            if !valid_module(name) || reserved(name) {
                return Err(format!("invalid or reserved worker module: {name}"));
            }
            if module.source.len() > 256 * 1024 {
                return Err(format!("{name}: module source exceeds 256 KiB"));
            }
            ruff_python_parser::parse_module(&module.source).map_err(|e| format!("{name}: {e}"))?;
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let modules: serde_json::Map<String, serde_json::Value> = self
            .modules
            .iter()
            .map(|(n, m)| {
                (
                    n.clone(),
                    serde_json::json!({"source":m.source,"package":m.package}),
                )
            })
            .collect();
        Ok(format!(
            "{PREFIX}{}",
            serde_json::json!({"entry":self.entry,"single":self.single,"modules":modules})
        ))
    }
    pub fn read(
        root: &Path,
        entry: &Path,
        extensions: &BTreeMap<String, SourceModule>,
    ) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let entry = root.join(entry).canonicalize().map_err(|e| e.to_string())?;
        if !entry.starts_with(&root) {
            return Err("Python entry must stay within its project".into());
        }
        let directory = entry.is_dir() || entry.file_name().is_some_and(|n| n == "__init__.py");
        let package_dir = if entry.is_dir() {
            entry.clone()
        } else {
            entry.parent().unwrap().to_owned()
        };
        let entry_file = if directory {
            package_dir.join("__init__.py")
        } else {
            entry.clone()
        };
        let mut base = if directory {
            package_dir
                .parent()
                .ok_or("package has no parent directory")?
                .to_owned()
        } else {
            package_dir.clone()
        };
        while base.starts_with(&root) && base.join("__init__.py").is_file() {
            let Some(parent) = base.parent() else { break };
            base = parent.to_owned();
        }
        let relative = if directory { &package_dir } else { &entry_file };
        let relative = relative.strip_prefix(&base).map_err(|e| e.to_string())?;
        let relative = if directory {
            relative.to_owned()
        } else {
            relative.with_extension("")
        };
        let name = relative
            .components()
            .map(|p| p.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join(".");
        let mut result = Self {
            entry: name.clone(),
            modules: BTreeMap::new(),
            single: !directory,
        };
        let mut pending = vec![(name.clone(), entry_file, directory)];
        if directory {
            collect(&package_dir, &name, &mut pending)?;
        }
        let mut parent = name.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('.') {
            if let Some((path, package)) = locate(&base, prefix) {
                pending.push((prefix.into(), path, package));
            }
            parent = prefix;
        }
        while let Some((name, path, package)) = pending.pop() {
            if let Some(existing) = result.modules.get(&name) {
                if existing.package != package {
                    return Err(format!(
                        "ambiguous Python module: {name} has both a .py file and a package"
                    ));
                }
                continue;
            }
            if extensions.contains_key(&name) || reserved(&name) {
                return Err(format!(
                    "worker module conflicts with a host module: {name}"
                ));
            }
            let path = path
                .canonicalize()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if !path.starts_with(&root) || !path.starts_with(&base) {
                return Err(format!(
                    "{}: Python source escapes the project import root",
                    path.display()
                ));
            }
            let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            if size > 256 * 1024 {
                return Err(format!("{name}: module source exceeds 256 KiB"));
            }
            let source = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let module = SourceModule {
                source,
                package,
                worker: true,
            };
            let imports = imports(&name, &module)?;
            result.modules.insert(name.clone(), module);
            if result.modules.len() > MAX_FILES
                || result
                    .modules
                    .values()
                    .map(|m| m.source.len())
                    .sum::<usize>()
                    > MAX_BYTES
            {
                return Err(
                    "Python packages are limited to 256 modules and 1 MiB of source".into(),
                );
            }
            for import in imports {
                if extensions.contains_key(&import) || reserved(&import) {
                    continue;
                }
                let mut prefix = String::new();
                for part in import.split('.') {
                    if !prefix.is_empty() {
                        prefix.push('.');
                    }
                    prefix.push_str(part);
                    if let Some((path, package)) = locate(&base, &prefix) {
                        pending.push((prefix.clone(), path, package));
                    }
                }
            }
        }
        result.validate()?;
        Ok(result)
    }
}
fn locate(base: &Path, name: &str) -> Option<(PathBuf, bool)> {
    let path = base.join(name.replace('.', "/"));
    if path.join("__init__.py").is_file() {
        Some((path.join("__init__.py"), true))
    } else if path.with_extension("py").is_file() {
        Some((path.with_extension("py"), false))
    } else {
        None
    }
}
fn collect(
    directory: &Path,
    name: &str,
    out: &mut Vec<(String, PathBuf, bool)>,
) -> Result<(), String> {
    if name.split('.').count() > 32 {
        return Err("Python packages are limited to 32 directory levels".into());
    }
    let entries = std::fs::read_dir(directory).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with('.') || file_name == "__pycache__" {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!(
                "{}: package sources cannot be symlinks",
                path.display()
            ));
        }
        if kind.is_dir() {
            collect(&path, &format!("{name}.{file_name}"), out)?;
        } else if path.extension().is_some_and(|e| e == "py") {
            let init = file_name == "__init__.py";
            if init && out.iter().any(|(_, existing, _)| existing == &path) {
                continue;
            }
            out.push((
                if init {
                    name.into()
                } else {
                    format!("{name}.{}", path.file_stem().unwrap().to_string_lossy())
                },
                path,
                init,
            ));
            if out.len() > MAX_FILES {
                return Err("Python packages are limited to 256 modules".into());
            }
        }
    }
    Ok(())
}
fn imports(name: &str, module: &SourceModule) -> Result<Vec<String>, String> {
    struct Imports(Vec<Stmt>);
    impl<'a> Visitor<'a> for Imports {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if matches!(stmt, Stmt::Import(_) | Stmt::ImportFrom(_)) {
                self.0.push(stmt.clone());
            }
            visitor::walk_stmt(self, stmt);
        }
    }
    let parsed =
        ruff_python_parser::parse_module(&module.source).map_err(|e| format!("{name}: {e}"))?;
    let mut visitor = Imports(Vec::new());
    visitor.visit_body(&parsed.syntax().body);
    let mut result = Vec::new();
    for stmt in visitor.0 {
        match stmt {
            Stmt::Import(i) => result.extend(i.names.into_iter().map(|a| a.name.to_string())),
            Stmt::ImportFrom(i) => {
                let target = absolute(name, module.package, i.module.as_deref(), i.level)?;
                result.push(target.clone());
                result.extend(i.names.into_iter().map(|a| format!("{target}.{}", a.name)));
            }
            _ => unreachable!(),
        }
    }
    Ok(result)
}
pub(crate) fn reserved(name: &str) -> bool {
    // Monty's built-in modules cannot be replaced by application code.
    matches!(
        name.split('.').next().unwrap_or(""),
        "celld"
            | "asyncio"
            | "base64"
            | "binascii"
            | "builtins"
            | "collections"
            | "dataclasses"
            | "datetime"
            | "functools"
            | "gc"
            | "itertools"
            | "json"
            | "math"
            | "os"
            | "pathlib"
            | "re"
            | "sys"
            | "typing"
            | "unicodedata"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Project(PathBuf);
    impl Project {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "pycelld-package-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, source: &str) {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        fn read(&self, name: &str) -> Result<Package, String> {
            Package::read(
                &self.0,
                Path::new(name),
                &crate::extensions::Extensions::default().sources(),
            )
        }
    }
    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn directories_and_init_files_export_a_deterministic_package() {
        let p = Project::new();
        p.write(
            "shop/__init__.py",
            "from .api import run\n__all__ = ['run']",
        );
        p.write(
            "shop/api.py",
            "from .values import answer\ndef run(): return answer",
        );
        p.write("shop/values.py", "answer = 42");
        p.write("shop/unused.py", "raise ValueError('not imported')");
        p.write("shop/assets/notes.txt", "not Python");
        let directory = p.read("shop").unwrap();
        assert_eq!(directory.entry, "shop");
        assert_eq!(directory.modules.len(), 4);
        let encoded = directory.encode().unwrap();
        assert_eq!(
            encoded,
            p.read("shop/__init__.py").unwrap().encode().unwrap()
        );
        assert_eq!(encoded, Package::parse(&encoded).unwrap().encode().unwrap());
        p.write("shop/values.py", "answer = 43");
        assert_ne!(encoded, p.read("shop").unwrap().encode().unwrap());
    }

    #[test]
    fn file_entries_follow_transitive_imports_and_package_parents() {
        let p = Project::new();
        p.write("shop/__init__.py", "");
        p.write("shop/api/__init__.py", "from .handlers import run");
        p.write(
            "shop/api/handlers.py",
            "from ..values import answer\ndef run(): return answer",
        );
        p.write("shop/values.py", "answer = 42");
        p.write("shop/unused.py", "unused = True");
        let files = p.read("shop/api/handlers.py").unwrap();
        assert_eq!(files.entry, "shop.api.handlers");
        assert_eq!(files.modules.len(), 4);
        assert!(!files.modules.contains_key("shop.unused"));
        assert_eq!(p.read("shop/api").unwrap().entry, "shop.api");
        p.write("worker.py", "from shop.api import run");
        assert_eq!(p.read("worker.py").unwrap().modules.len(), 5);
    }

    #[test]
    fn packaging_rejects_missing_oversize_ambiguous_and_reserved_sources() {
        let p = Project::new();
        p.write("empty/module.py", "");
        assert!(p.read("empty").is_err());
        p.write("shop/__init__.py", "");
        p.write("shop/large.py", &" ".repeat(256 * 1024 + 1));
        assert!(p.read("shop").err().unwrap().contains("256 KiB"));
        std::fs::remove_file(p.0.join("shop/large.py")).unwrap();
        p.write("shop/foo.py", "");
        p.write("shop/foo/__init__.py", "");
        assert!(p.read("shop").err().unwrap().contains("ambiguous"));
        p.write("celld/__init__.py", "");
        assert!(p.read("celld").err().unwrap().contains("conflicts"));
        assert!(Package::parse(&format!("{PREFIX}{}", serde_json::json!({"entry":"../escape", "modules":{"../escape":{"source":"", "package":false}}}))).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn packages_cannot_follow_symlinks_or_import_outside_the_project() {
        let p = Project::new();
        let outside = Project::new();
        p.write("shop/__init__.py", "");
        outside.write("secret.py", "secret = 1");
        std::os::unix::fs::symlink(outside.0.join("secret.py"), p.0.join("shop/secret.py"))
            .unwrap();
        assert!(p.read("shop").err().unwrap().contains("symlinks"));
        std::os::unix::fs::symlink(outside.0.join("secret.py"), p.0.join("secret.py")).unwrap();
        p.write(
            "worker.py",
            "import secret\ndef run(): return secret.secret",
        );
        assert!(p.read("worker.py").err().unwrap().contains("escapes"));
    }
}
