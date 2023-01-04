use test_binary::build_test_binary;

pub enum Application {
    RustFileOps,
}

impl Application {
    pub fn get_executable(&self) -> String {
        match self {
            Application::RustFileOps => String::from("target/debug/fileops"),
        }
    }

    pub fn build_executable(&self) -> Self {
        match self {
            Application::RustFileOps => {
                let test_bin_path = build_test_binary("fileops", "applications")
                    .expect("error building test binary");
                let test_bin_path = test_bin_path
                    .to_str()
                    .expect("error converting test binary path to string")
                    .to_string();
                println!("test_bin_path: {}", test_bin_path);
                Application::RustFileOps
            }
        }
    }
}
