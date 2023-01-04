use std::{collections::HashMap, process::Stdio, sync::Arc};

use k8s_openapi::chrono::Utc;
use tokio::{
    io::{AsyncReadExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
};

pub struct TestProcess<'a> {
    pub child: Option<Child>,
    stderr: Arc<Mutex<String>>,
    stdout: Arc<Mutex<String>>,
    env: HashMap<&'a str, &'a str>,
}

impl<'a> TestProcess<'a> {
    pub async fn get_stdout(&self) -> String {
        (*self.stdout.lock().await).clone()
    }

    pub async fn assert_stderr_empty(&self) {
        assert!((*self.stderr.lock().await).is_empty());
    }

    fn from_child(mut child: Child) -> TestProcess<'a> {
        let stderr_data = Arc::new(Mutex::new(String::new()));
        let stdout_data = Arc::new(Mutex::new(String::new()));
        let child_stderr = child.stderr.take().unwrap();
        let child_stdout = child.stdout.take().unwrap();
        let stderr_data_reader = stderr_data.clone();
        let stdout_data_reader = stdout_data.clone();
        let pid = child.id().unwrap();

        tokio::spawn(async move {
            let mut reader = BufReader::new(child_stderr);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }

                let string = String::from_utf8_lossy(&buf[..n]);
                eprintln!("stderr {:?} {pid}: {}", Utc::now(), string);
                {
                    (*stderr_data_reader.lock().await).push_str(&string);
                }
            }
        });
        tokio::spawn(async move {
            let mut reader = BufReader::new(child_stdout);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                let string = String::from_utf8_lossy(&buf[..n]);
                print!("stdout {:?} {pid}: {}", Utc::now(), string);
                {
                    (*stdout_data_reader.lock().await).push_str(&string);
                }
            }
        });

        TestProcess {
            child: Some(child),
            stderr: stderr_data,
            stdout: stdout_data,
            env: HashMap::new(),
        }
    }

    pub async fn start_process(
        executable: String,
        args: Vec<String>,
        env: HashMap<&str, &str>,
    ) -> TestProcess<'a> {
        let child = Command::new(executable)
            .args(args)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        TestProcess::from_child(child)
    }

    pub async fn assert_stdout_contains(&self, string: &str) {
        assert!((*self.stdout.lock().await).contains(string));
    }
}

pub trait EnvProvider<'b> {
    fn with_basic_env(&mut self);
    fn with_custom_env(&mut self, custom_env: HashMap<&'b str, &'b str>);
}

impl<'a> EnvProvider<'a> for TestProcess<'a> {
    fn with_basic_env(&mut self) {
        self.env.insert("MIRRORD_PROGRESS_MODE", "off");
        self.env.insert("RUST_LOG", "warn,mirrord=trace");
        self.env
            .insert("MIRRORD_IMPERSONATED_TARGET", "pod/mock-target");
        self.env.insert("MIRRORD_REMOTE_DNS", "false");
    }

    fn with_custom_env(&mut self, custom_env: HashMap<&'a str, &'a str>) {
        self.env.extend(custom_env);
    }
}

pub trait MirrordBinaryProvider<'b> {
    fn get_mirrord_binary(&self) -> String;
}

impl<'a> MirrordBinaryProvider<'a> for TestProcess<'a> {
    fn get_mirrord_binary(&self) -> String {
        let current_exe =
            std::env::current_exe().expect("Failed to get the path of the integration test binary");
        current_exe.to_string_lossy().to_string()
    }
}
