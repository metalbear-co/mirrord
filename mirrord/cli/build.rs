/// Ensure `packages/monitor/dist` exists so rust-embed doesn't fail during compilation.
fn ensure_monitor_frontend_dist() {
    let dist_dir = std::path::Path::new("../../packages/monitor/dist");
    if !dist_dir.exists() {
        std::fs::create_dir_all(dist_dir).ok();
    }
}

fn recheck_and_setup_layer_file() {
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE");

    #[cfg(target_os = "macos")]
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE_MACOS_ARM64");

    if std::env::var("MIRRORD_LAYER_FILE_MACOS_ARM64").is_err()
        && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|target| target == "macos")
    {
        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE_MACOS_ARM64={}",
            std::env::var("CARGO_MANIFEST_PATH").unwrap()
        );
    }
}

#[cfg(feature = "wizard")]
fn build_wizard_frontend() {
    use std::{env, path::Path, process::Command};

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let out_dir = Path::new(&out_dir);

    println!("cargo::rerun-if-env-changed=WIZARD_DIST_DIR");

    let dist_path = if let Ok(frontend_dist_override) = env::var("WIZARD_DIST_DIR") {
        let dist = Path::new(&frontend_dist_override).to_path_buf();
        println!("cargo::rerun-if-changed={}", dist.display());
        dist
    } else {
        let input_path = Path::new("../../packages/wizard");
        let dist_path = out_dir.join("dist");

        println!("cargo::rerun-if-changed={}", input_path.display());
        println!("cargo::rerun-if-changed=.");

        let status = Command::new("npm")
            .args(["install"])
            .current_dir(input_path)
            .status()
            .expect("npm install command should finish");
        assert!(status.success(), "npm install command should succeed");

        let status = Command::new("npm")
            .args([
                "run",
                "build",
                "--",
                "--emptyOutDir",
                "--outDir",
                &dist_path.display().to_string(),
            ])
            .current_dir(input_path)
            .status()
            .expect("npm build command should finish");
        assert!(status.success(), "npm build command should succeed");
        dist_path
    };

    let tar_path = out_dir.join("wizard-frontend.tar.gz");
    let mut tar_command = Command::new("tar");
    let status = tar_command
        .arg("czf")
        .arg(tar_path)
        .arg("--directory")
        .arg(dist_path)
        .arg(".")
        .status()
        .expect("tar command should finish");
    assert!(status.success(), "tar command should succeed");
}

fn package_sip_binaries() {
    use std::{env, path::Path};

    println!("cargo::rerun-if-env-changed=MIRRORD_SIP_BINARIES_TAR");

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let out_dir = Path::new(&out_dir);
    let tar_path = out_dir.join("apple-utils.tar.gz");

    match env::var("MIRRORD_SIP_BINARIES_TAR") {
        Ok(source_path) => {
            println!("cargo::rerun-if-changed={source_path}");
            std::fs::copy(&source_path, &tar_path)
                .expect("copying embedded SIP utilities bundle should succeed");
        }
        Err(_) => {
            if !std::fs::exists(&tar_path).unwrap_or(false) {
                std::fs::write(&tar_path, "")
                    .expect("creating placeholder SIP archive should work");
            }
        }
    }
}

fn main() {
    if std::env::var("CLIPPY_ARGS").is_ok() {
        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
            std::env::var("CARGO_MANIFEST_PATH").unwrap()
        );
        println!(
            "cargo:rustc-env=MIRRORD_LAYER_FILE_MACOS_ARM64={}",
            std::env::var("CARGO_MANIFEST_PATH").unwrap()
        );

        let frontend_path = format!(
            "{}/wizard-frontend.tar.gz",
            std::env::var("OUT_DIR").unwrap()
        );

        if !std::fs::exists(&frontend_path).unwrap_or(false) {
            std::fs::write(frontend_path, "").unwrap();
        };

        if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|target| target == "macos") {
            let sip_path = format!("{}/apple-utils.tar.gz", std::env::var("OUT_DIR").unwrap());

            if !std::fs::exists(&sip_path).unwrap_or(false) {
                std::fs::write(sip_path, "").unwrap();
            };
        }

        ensure_monitor_frontend_dist();

        return;
    }

    recheck_and_setup_layer_file();

    #[cfg(feature = "wizard")]
    build_wizard_frontend();

    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|target| target == "macos") {
        package_sip_binaries();
    }

    ensure_monitor_frontend_dist();

    if std::env::var("MIRRORD_LAYER_FILE_MACOS_ARM64").is_err()
        && std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|t| t.eq("aarch64") || t.eq("x86_64"))
        && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|t| t.eq("macos"))
    {
        println!(
            "cargo::warning=No environment variable 'MIRRORD_LAYER_FILE_MACOS_ARM64' found - using a placeholder path; release builds should provide the compiled `aarch64-apple-darwin` layer"
        );
    };
}
