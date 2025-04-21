/// Runs a script and then a binary both with DYLD_PRINT_ENV set.
/// With `skip_sip` set, both processes stderr should not contain DYLD_PRINT_ENV
/// or any other DYLD_* environment variables.
fn main() {
    let script_path = std::env::var("TEST_SCRIPT_PATH").unwrap();
    let binary_path = std::env::var("TEST_BINARY_PATH").unwrap();
    let output = std::process::Command::new(script_path)
        .env("DYLD_PRINT_ENV", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    if stderr.contains("DYLD_PRINT_ENV") {
        panic!("stderr shouldn't contain `DYLD_PRINT_ENV`. stderr:\n{stderr}");
    }

    let output = std::process::Command::new(binary_path)
        .env("DYLD_PRINT_ENV", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    if stderr.contains("DYLD_PRINT_ENV") {
        panic!("stderr shouldn't contain `DYLD_PRINT_ENV`. stderr:\n{stderr}");
    }
}
