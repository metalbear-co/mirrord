fn main() {
    let script_path = std::env::var("TEST_SCRIPT_PATH").unwrap();
    let binary_path = std::env::var("TEST_BINARY_PATH").unwrap();
    let _output = std::process::Command::new(script_path).output().unwrap();

    let _output = std::process::Command::new(binary_path).output().unwrap();
}
