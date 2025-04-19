use std::io::Write;

/// Runs a bash script and then the `ls` command both with DYLD_PRINT_ENV set.
/// With `skip_sip` set to `bash;ls`, both processes stderr should not contain DYLD_PRINT_ENV
/// or any other DYLD_* environment variables.
fn main() {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), "../nothing.sh");
    let output = std::process::Command::new(path)
        .env("DYLD_PRINT_ENV", "1")
        .output()
        .unwrap();
    std::io::stderr().write_all(&output.stderr).unwrap();

    let output = std::process::Command::new("ls")
        .env("DYLD_PRINT_ENV", "1")
        .output()
        .unwrap();
    std::io::stderr().write_all(&output.stderr).unwrap();
}
