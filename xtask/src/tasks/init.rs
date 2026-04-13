use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use which::which;

/// Bootstraps pinned external tools used by xtask so CI and local builds share one setup path.
pub fn run() -> Result<()> {
    let python = PythonLauncher::detect()?;
    let uv = require_uv()?;
    sync_python_tools(&python, &uv)?;
    install_wrapper(
        "cargo-zigbuild",
        &venv_executable(&python_tools_environment()?, "cargo-zigbuild"),
    )?;

    println!("✓ xtask dependencies are ready");
    Ok(())
}

#[derive(Clone)]
struct PythonLauncher {
    program: PathBuf,
    args: Vec<String>,
}

impl PythonLauncher {
    fn detect() -> Result<Self> {
        #[cfg(windows)]
        {
            if let Ok(program) = which("py") {
                return Ok(Self {
                    program,
                    args: vec!["-3".to_owned()],
                });
            }
        }

        for candidate in ["python3", "python"] {
            if let Ok(program) = which(candidate) {
                return Ok(Self {
                    program,
                    args: Vec::new(),
                });
            }
        }

        bail!("Python 3 is required for `cargo xtask init`");
    }

    fn executable(&self) -> Result<PathBuf> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        cmd.args(["-c", "import sys; print(sys.executable)"]);
        Ok(PathBuf::from(command_stdout(&mut cmd)?.trim()))
    }
}

fn require_uv() -> Result<PathBuf> {
    which("uv")
        .context("`uv` is required for `cargo xtask init`. Install it before running this command.")
}

fn sync_python_tools(python: &PythonLauncher, uv: &Path) -> Result<()> {
    let project_dir = python_tools_project_dir();
    let environment = python_tools_environment()?;
    let tool_root = tool_root()?;

    fs::create_dir_all(&tool_root)
        .with_context(|| format!("Failed to create {}", tool_root.display()))?;

    println!(
        "Syncing xtask Python tools from {}...",
        project_dir.display()
    );

    let mut cmd = Command::new(uv);
    cmd.args(["sync", "--project"])
        .arg(&project_dir)
        .args(["--no-dev", "--no-install-project", "--python"])
        .arg(python.executable()?)
        .env("UV_PROJECT_ENVIRONMENT", &environment);
    run_command(&mut cmd, "Failed to sync xtask Python tools")
}

fn install_wrapper(name: &str, target: &Path) -> Result<()> {
    let bin_dir = cargo_home()?.join("bin");
    fs::create_dir_all(&bin_dir)
        .with_context(|| format!("Failed to create {}", bin_dir.display()))?;

    #[cfg(windows)]
    {
        let wrapper = bin_dir.join(format!("{name}.cmd"));
        let script = format!("@echo off\r\n\"{}\" %*\r\n", target.display());
        fs::write(&wrapper, script)
            .with_context(|| format!("Failed to write {}", wrapper.display()))?;
        println!("✓ Installed wrapper at {}", wrapper.display());
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;

        let wrapper = bin_dir.join(name);
        let script = format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", target.display());
        fs::write(&wrapper, script)
            .with_context(|| format!("Failed to write {}", wrapper.display()))?;
        let mut permissions = fs::metadata(&wrapper)
            .with_context(|| format!("Failed to stat {}", wrapper.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions)
            .with_context(|| format!("Failed to chmod {}", wrapper.display()))?;
        println!("✓ Installed wrapper at {}", wrapper.display());
    }

    Ok(())
}

fn venv_executable(venv_dir: &Path, executable: &str) -> PathBuf {
    #[cfg(windows)]
    {
        venv_dir.join("Scripts").join(format!("{executable}.exe"))
    }

    #[cfg(not(windows))]
    {
        venv_dir.join("bin").join(executable)
    }
}

fn cargo_home() -> Result<PathBuf> {
    if let Some(cargo_home) = env::var_os("CARGO_HOME") {
        return Ok(PathBuf::from(cargo_home));
    }

    home_dir().map(|home| home.join(".cargo"))
}

fn tool_root() -> Result<PathBuf> {
    Ok(cargo_home()?.join("mirrord-xtask"))
}

fn python_tools_environment() -> Result<PathBuf> {
    Ok(tool_root()?.join("python-tools"))
}

fn python_tools_project_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest dir should have a repository root parent")
        .to_path_buf()
}

fn home_dir() -> Result<PathBuf> {
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }

    #[cfg(windows)]
    if let Some(home) = env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(home));
    }

    bail!("Failed to determine the current user's home directory")
}

fn run_command(cmd: &mut Command, error: &str) -> Result<()> {
    let status = cmd.status().with_context(|| error.to_owned())?;
    if !status.success() {
        bail!("{error}");
    }
    Ok(())
}

fn command_stdout(cmd: &mut Command) -> Result<String> {
    let output = cmd.output().context("Failed to run command")?;
    if !output.status.success() {
        bail!(
            "{}",
            String::from_utf8_lossy(&output.stderr).trim().to_owned()
        );
    }
    String::from_utf8(output.stdout).context("Command output was not valid UTF-8")
}
