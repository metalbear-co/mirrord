/// Ensure `packages/ui/dist` exists so rust-embed doesn't fail during compilation.
///
/// `packages/ui` is the merged frontend (session monitor + config wizard). It is built with
/// `pnpm --filter mirrord-ui build` before the Rust build; this only creates an empty placeholder
/// so a `cargo` build without a prior frontend build still compiles.
fn ensure_ui_frontend_dist() {
    let dist_dir = std::path::Path::new("../../packages/ui/dist");
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

        if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|target| target == "macos") {
            let sip_path = format!("{}/apple-utils.tar.gz", std::env::var("OUT_DIR").unwrap());

            if !std::fs::exists(&sip_path).unwrap_or(false) {
                std::fs::write(sip_path, "").unwrap();
            };
        }

        ensure_ui_frontend_dist();

        return;
    }

    recheck_and_setup_layer_file();

    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|target| target == "macos") {
        package_sip_binaries();
    }

    ensure_ui_frontend_dist();

    if std::env::var("MIRRORD_LAYER_FILE_MACOS_ARM64").is_err()
        && std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|t| t.eq("aarch64") || t.eq("x86_64"))
        && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|t| t.eq("macos"))
    {
        println!(
            "cargo::warning=No environment variable 'MIRRORD_LAYER_FILE_MACOS_ARM64' found - using a placeholder path; release builds should provide the compiled `aarch64-apple-darwin` layer"
        );
    };
}
