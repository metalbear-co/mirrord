#[cfg(target_os = "windows")]
use std::path::Path;
use std::process::exit;

fn recheck_and_setup_layer_file() {
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE");

    #[cfg(target_os = "macos")]
    println!("cargo::rerun-if-env-changed=MIRRORD_LAYER_FILE_MACOS_ARM64");

    if std::env::var("MIRRORD_LAYER_FILE").is_err() {
        #[cfg(target_os = "windows")]
        {
            let out_dir_env =
                std::env::var("OUT_DIR").expect("Failed getting OUT_DIR from environment");
            let out_dir = Path::new(&out_dir_env);
            let build_dir = out_dir
                .ancestors()
                .nth(3)
                .expect("Failed extracting build directory from OUT_DIR");
            let layer = build_dir.join("mirrord_layer_win.dll");

            println!(
                "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
                layer.to_str().unwrap()
            );
        }

        #[cfg(not(target_os = "windows"))]
        {
            println!(
                "cargo:rustc-env=MIRRORD_LAYER_FILE={}",
                std::env::var("CARGO_CDYLIB_FILE_MIRRORD_LAYER").unwrap()
            );
        }
    };
}

#[cfg(feature = "wizard")]
fn build_wizard_frontend() {
    use std::{env, path::Path, process::Command};

    let input_path = Path::new("../../wizard-frontend");
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let out_dir = Path::new(&out_dir);
    let tar_path = out_dir.join("wizard-frontend.tar.gz");
    let dist_path = out_dir.join("dist");

    // rerun if the wizard frontend has any changes
    println!("cargo::rerun-if-changed={}", input_path.display());
    // restore default behaviour (see
    // https://doc.rust-lang.org/cargo/reference/build-scripts.html#rerun-if-changed)
    println!("cargo::rerun-if-changed=.");

    let mut npm_install_command = Command::new("bash");
    let status = npm_install_command
        .args(["-c", "npm install"])
        .current_dir(input_path)
        .status()
        .expect("npm install command should finish");
    assert!(status.success(), "npm install command should succeed");

    let mut npm_build_command = Command::new("bash");
    let build_command_string = format!("npm run build -- --emptyOutDir --outDir {}", &dist_path.display());
    let status = npm_build_command
        .args(["-c", &build_command_string])
        .current_dir(input_path)
        .status()
        .expect("npm build command should finish");
    assert!(status.success(), "npm build command should succeed");

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

fn main() {
    // Make sure `MIRRORD_LAYER_FILE` is provided either by user, or computed.
    recheck_and_setup_layer_file();

    // Build the wizard frontend
    #[cfg(feature = "wizard")]
    build_wizard_frontend();

    // this check uses cargo env vars instead of conditional compilation due to cfg! not respecting
    // the target flag on a build
    if std::env::var("MIRRORD_LAYER_FILE_MACOS_ARM64").is_err()
        && std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|t| t.eq("aarch64") || t.eq("x86_64"))
        && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|t| t.eq("macos"))
    {
        println!(
            "cargo::warning=No environment variable 'MIRRORD_LAYER_FILE_MACOS_ARM64' found - it should contain the path to the mirrord layer compiled for the `aarch64-apple-darwin` target"
        );
        exit(1);
    };
}
