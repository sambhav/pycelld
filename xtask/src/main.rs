//! Reproduce a patched upstream workspace without keeping a hosted fork.
use std::{
    env,
    error::Error,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let args = env::args().skip(1).collect::<Vec<_>>();
    let Some(action) = args.first().map(String::as_str) else {
        return Err("usage: cargo xtask <prepare|build|test> [Cargo options]".into());
    };
    if !matches!(action, "prepare" | "build" | "test") {
        return Err(format!("unknown command: {action}").into());
    }
    fs::create_dir_all(root.join("target"))?;
    // Hold this through the build so a second invocation cannot replace the
    // generated workspace while Cargo is compiling it.
    let _guard = PrepareLock::acquire(root.join("target/celld-prepare.lock"))?;
    let source = prepare(root)?;
    if action == "prepare" {
        return Ok(());
    }
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    if action == "test" {
        run(Command::new(&cargo)
            .current_dir(root)
            .args(["test", "--workspace", "--locked"]))?;
        run(Command::new(&cargo).current_dir(root).args([
            "test",
            "--manifest-path",
            "xtask/Cargo.toml",
            "--locked",
        ]))?;
        run(Command::new(&cargo).current_dir(root).args([
            "test",
            "--manifest-path",
            "tools/packing-checks/Cargo.toml",
            "--locked",
        ]))?;
    }
    let mut command = Command::new(cargo);
    command
        .current_dir(&source)
        .args([action, "-p", "celld", "--features", "monty", "--locked"])
        .arg("--target-dir")
        .arg(root.join("target"));
    if action == "test" {
        command.args([
            "--lib",
            "--test",
            "monty_native",
            "--test",
            "native_extension",
        ]);
    }
    if !args[1..]
        .iter()
        .any(|a| a == "--release" || a == "--profile" || a.starts_with("--profile="))
    {
        command.args(["--profile", "lab"]);
    }
    command.args(&args[1..]);
    run(&mut command)
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{command:?} exited with {status}").into());
    }
    Ok(())
}
fn output(command: &mut Command) -> Result<String> {
    let result = command.output()?;
    if !result.status.success() {
        return Err(format!("{command:?}: {}", String::from_utf8_lossy(&result.stderr)).into());
    }
    Ok(String::from_utf8(result.stdout)?.trim().to_owned())
}
fn git(path: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(path);
    command
}
fn hash(data: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(data)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err("git hash-object failed".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
fn patches(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(root.join("patches"))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|p| p.extension().is_some_and(|e| e == "patch"));
    files.sort();
    if files.is_empty() {
        return Err("patches/ contains no patch files".into());
    }
    Ok(files)
}
fn state(path: &Path) -> Result<(String, String)> {
    let state = fs::read_to_string(path.join(".git/celld-prepared"))
        .map_err(|_| "refusing to replace a checkout not created by cargo xtask")?;
    let (fingerprint, tree) = state
        .trim()
        .split_once('\n')
        .ok_or("invalid preparation state")?;
    Ok((fingerprint.into(), tree.into()))
}
fn clean(path: &Path, tree: &str) -> Result<()> {
    // Compare with the prepared tree, including staged edits. A developer's
    // changes must never disappear just because the upstream pin was updated.
    if !output(git(path).args(["diff", "--cached", "--name-only", tree, "--"]))?.is_empty()
        || !output(git(path).args(["diff", "--name-only", tree, "--"]))?.is_empty()
        || !output(git(path).args(["ls-files", "--others", "--exclude-standard"]))?.is_empty()
    {
        return Err(format!(
            "{} has local changes; save them as a patch before preparing again",
            path.display()
        )
        .into());
    }
    Ok(())
}
fn prepare(root: &Path) -> Result<PathBuf> {
    let source = fs::read_to_string(root.join("upstream/repository"))?
        .trim()
        .to_owned();
    let revision = fs::read_to_string(root.join("upstream/revision"))?
        .trim()
        .to_owned();
    if revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("upstream/revision must be a full Git commit SHA".into());
    }
    let files = patches(root)?;
    let lock = fs::read(root.join("upstream/Cargo.lock"))?;
    let mut inputs = format!("celld-prepare-v1\0{source}\0{revision}\0").into_bytes();
    for path in &files {
        inputs.extend_from_slice(path.file_name().unwrap().as_encoded_bytes());
        inputs.push(0);
        let data = fs::read(path)?;
        inputs.extend_from_slice(&data.len().to_le_bytes());
        inputs.extend_from_slice(&data);
    }
    inputs.extend_from_slice(&lock);
    let fingerprint = hash(&inputs)?;
    let target = root.join("target");
    fs::create_dir_all(&target)?;
    let checkout = target.join("celld");
    if checkout.exists() {
        let (previous, tree) = state(&checkout)?;
        clean(&checkout, &tree)?;
        if previous == fingerprint {
            return Ok(checkout);
        }
    }
    let staging = target.join("celld-preparing");
    if staging.exists() {
        return Err(
            "target/celld-preparing exists; inspect it before removing it and retrying".into(),
        );
    }
    fs::create_dir(&staging)?;
    let result = (|| -> Result<()> {
        run(git(&staging).args(["init", "--quiet"]))?;
        run(git(&staging).args(["remote", "add", "origin", &source]))?;
        run(git(&staging).args(["fetch", "--quiet", "--depth=1", "origin", &revision]))?;
        run(git(&staging).args(["checkout", "--quiet", "--detach", "FETCH_HEAD"]))?;
        if output(git(&staging).args(["rev-parse", "HEAD"]))? != revision {
            return Err("fetched upstream revision does not match the pin".into());
        }
        for patch in &files {
            eprintln!("Applying {}", patch.file_name().unwrap().to_string_lossy());
            run(git(&staging).args(["apply", "--check"]).arg(patch))?;
            run(git(&staging).arg("apply").arg(patch))?;
        }
        fs::write(staging.join("Cargo.lock"), &lock)?;
        run(git(&staging).args(["add", "--all"]))?;
        let tree = output(git(&staging).arg("write-tree"))?;
        fs::write(
            staging.join(".git/celld-prepared"),
            format!("{fingerprint}\n{tree}\n"),
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        fs::remove_dir_all(&staging)?;
        return Err(error);
    }
    if checkout.exists() {
        fs::remove_dir_all(&checkout)?;
    }
    fs::rename(staging, &checkout)?;
    eprintln!("Prepared celld {revision}");
    Ok(checkout)
}
struct PrepareLock {
    _file: fs::File,
}
impl PrepareLock {
    fn acquire(path: PathBuf) -> Result<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        file.try_lock().map_err(|e| {
            format!(
                "cannot lock {}: {e}; another build may be running",
                path.display()
            )
        })?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepares_without_a_fork_and_preserves_edits_and_previous_checkout_on_failure() -> Result<()>
    {
        let root = env::temp_dir().join(format!(
            "celld-xtask-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir_all(root.join("origin"))?;
        fs::create_dir(root.join("upstream"))?;
        fs::create_dir(root.join("patches"))?;
        let origin = root.join("origin");
        run(git(&origin).args(["init", "--quiet"]))?;
        fs::write(origin.join("file"), "original\n")?;
        run(git(&origin).args(["add", "file"]))?;
        run(git(&origin).args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ]))?;
        let revision = output(git(&origin).args(["rev-parse", "HEAD"]))?;
        fs::write(root.join("upstream/repository"), origin.to_str().unwrap())?;
        fs::write(root.join("upstream/revision"), &revision)?;
        fs::write(root.join("upstream/Cargo.lock"), "test lock\n")?;
        let patch = root.join("patches/0001-test.patch");
        fs::write(
            &patch,
            "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-original\n+patched\n",
        )?;
        let lock_path = root.join("build.lock");
        let guard = PrepareLock::acquire(lock_path.clone())?;
        assert!(PrepareLock::acquire(lock_path.clone()).is_err());
        drop(guard);
        drop(PrepareLock::acquire(lock_path)?);
        let checkout = prepare(&root)?;
        assert_eq!(fs::read_to_string(checkout.join("file"))?, "patched\n");
        assert_eq!(
            output(git(&checkout).args(["rev-parse", "HEAD"]))?,
            revision
        );
        // An unchanged checkout needs no network/source access.
        fs::rename(&origin, root.join("origin-away"))?;
        assert_eq!(prepare(&root)?, checkout);
        fs::rename(root.join("origin-away"), &origin)?;
        fs::write(checkout.join("file"), "local edit\n")?;
        run(git(&checkout).args(["add", "file"]))?;
        assert!(
            prepare(&root)
                .unwrap_err()
                .to_string()
                .contains("local changes")
        );
        // Restoring only the worktree must not hide a staged edit.
        fs::write(checkout.join("file"), "patched\n")?;
        assert!(prepare(&root).is_err());
        let (_, tree) = state(&checkout)?;
        run(git(&checkout).args(["restore", "--source", &tree, "--staged", "--worktree", "."]))?;
        fs::write(checkout.join("untracked"), "keep me")?;
        assert!(prepare(&root).is_err());
        fs::remove_file(checkout.join("untracked"))?;
        fs::write(
            root.join("patches/0002-broken.patch"),
            "this is not a patch",
        )?;
        assert!(prepare(&root).is_err());
        assert_eq!(fs::read_to_string(checkout.join("file"))?, "patched\n");
        assert!(!root.join("target/celld-preparing").exists());
        fs::remove_file(root.join("patches/0002-broken.patch"))?;
        // A changed lock is an input and regenerates the workspace.
        fs::write(root.join("upstream/Cargo.lock"), "new lock\n")?;
        prepare(&root)?;
        assert_eq!(
            fs::read_to_string(checkout.join("Cargo.lock"))?,
            "new lock\n"
        );
        fs::write(root.join("upstream/revision"), "main")?;
        assert!(
            prepare(&root)
                .unwrap_err()
                .to_string()
                .contains("full Git commit SHA")
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
