use std::path::PathBuf;

fn main() {
    // println!("cargo:rerun-if-changed=exports.def");

    // let def_path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("exports.def");
    // println!("cargo:rustc-link-arg=-Wl,{}", def_path.display());
    // // println!("cargo:rustc-link-arg=-Lnative=target/release/build/aws-lc-sys-72ef32f20fbdfd6a/out/
    // // build/artifacts");
    println!("cargo:rustc-link-search=native=target/release/build/aws-lc-sys-72ef32f20fbdfd6a/out/build/artifacts");
    // println!("cargo:rustc-link-lib=stdc++");
}
