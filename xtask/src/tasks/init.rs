use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use which::which;

use super::versions;

/// Bootstraps pinned external tools used by xtask so CI and local builds share one setup path.
pub fn run() -> Result<()> {
    let python = PythonLauncher::detect()?;
    let uv = ensure_uv(&python)?;

    if let Some(hash) = versions::cargo_zigbuild_hash() {
        install_python_tool(
            &python,
            &uv,
            "cargo-zigbuild",
            versions::CARGO_ZIGBUILD_VERSION,
            hash,
            "cargo-zigbuild",
        )?;
    } else {
        println!(
            "Skipping cargo-zigbuild install on unsupported host {} {}",
            env::consts::OS,
            env::consts::ARCH
        );
    }

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

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        cmd
    }

    fn executable(&self) -> Result<PathBuf> {
        let mut cmd = self.command();
        cmd.args(["-c", "import sys; print(sys.executable)"]);
        Ok(PathBuf::from(command_stdout(&mut cmd)?.trim()))
    }
}

enum UvLauncher {
    Binary(PathBuf),
    PythonModule(PythonLauncher),
}

impl UvLauncher {
    fn command(&self) -> Command {
        match self {
            UvLauncher::Binary(path) => Command::new(path),
            UvLauncher::PythonModule(python) => {
                let mut cmd = python.command();
                cmd.args(["-m", "uv"]);
                cmd
            }
        }
    }
}

fn ensure_uv(python: &PythonLauncher) -> Result<UvLauncher> {
    if let Ok(path) = which("uv") {
        let mut version = Command::new(&path);
        version.arg("--version");
        if command_stdout(&mut version)?
            .trim()
            .starts_with(&format!("uv {}", versions::UV_VERSION))
        {
            println!("Using uv {}", versions::UV_VERSION);
            return Ok(UvLauncher::Binary(path));
        }
    }

    let hash = versions::uv_hash().with_context(|| {
        format!(
            "Unsupported host for pinned uv install: {} {}",
            env::consts::OS,
            env::consts::ARCH
        )
    })?;

    let requirements_dir = tool_root()?.join("requirements");
    fs::create_dir_all(&requirements_dir)
        .with_context(|| format!("Failed to create {}", requirements_dir.display()))?;

    let requirements = requirements_dir.join("uv.txt");
    write_hashed_requirement(&requirements, "uv", versions::UV_VERSION, hash)?;

    println!("Installing uv {}...", versions::UV_VERSION);
    let mut cmd = python.command();
    cmd.args([
        "-m",
        "pip",
        "install",
        "--user",
        "--disable-pip-version-check",
        "--only-binary=:all:",
        "--require-hashes",
        "-r",
    ])
    .arg(&requirements);
    run_command(&mut cmd, "Failed to install uv")?;

    let mut verify = python.command();
    verify.args(["-m", "uv", "--version"]);
    let version = command_stdout(&mut verify)?;
    if !version
        .trim()
        .starts_with(&format!("uv {}", versions::UV_VERSION))
    {
        bail!(
            "Installed uv does not match pinned version {}: {}",
            versions::UV_VERSION,
            version.trim()
        );
    }

    Ok(UvLauncher::PythonModule(python.clone()))
}

fn install_python_tool(
    python: &PythonLauncher,
    uv: &UvLauncher,
    package: &str,
    version: &str,
    hash: &str,
    executable: &str,
) -> Result<()> {
    let install_root = tool_root()?.join(format!("{package}-{version}"));
    let venv_dir = install_root.join("venv");
    let requirements = install_root.join("requirements.txt");
    let tool_executable = venv_executable(&venv_dir, executable);

    fs::create_dir_all(&install_root)
        .with_context(|| format!("Failed to create {}", install_root.display()))?;
    write_hashed_requirement(&requirements, package, version, hash)?;

    if !tool_executable.is_file() {
        println!("Installing {package} {version}...");

        let mut venv = uv.command();
        venv.arg("venv")
            .arg(&venv_dir)
            .arg("--python")
            .arg(python.executable()?);
        run_command(&mut venv, &format!("Failed to create venv for {package}"))?;

        let mut install = uv.command();
        install
            .args(["pip", "install", "--python"])
            .arg(venv_python(&venv_dir))
            .args(["--only-binary=:all:", "--require-hashes", "-r"])
            .arg(&requirements);
        run_command(&mut install, &format!("Failed to install {package}"))?;
    }

    install_wrapper(executable, &tool_executable)?;
    Ok(())
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

fn write_hashed_requirement(path: &Path, package: &str, version: &str, hash: &str) -> Result<()> {
    let contents = format!("{package}=={version} --hash=sha256:{hash}\n");
    fs::write(path, contents).with_context(|| format!("Failed to write {}", path.display()))
}

fn venv_executable(venv_dir: &Path, executable: &str) -> PathBuf {
    #[cfg(windows)]
    {
        return venv_dir.join("Scripts").join(format!("{executable}.exe"));
    }

    #[cfg(not(windows))]
    {
        venv_dir.join("bin").join(executable)
    }
}

fn venv_python(venv_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        return venv_dir.join("Scripts").join("python.exe");
    }

    #[cfg(not(windows))]
    {
        venv_dir.join("bin").join("python")
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
