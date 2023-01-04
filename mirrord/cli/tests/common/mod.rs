use std::{collections::HashMap, process::Stdio, sync::Arc};

use actix_codec::Framed;
use k8s_openapi::chrono::Utc;
use mirrord_protocol::DaemonCodec;
use test_binary::build_test_binary;
use tokio::{
    io::{AsyncReadExt, BufReader},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    sync::Mutex,
};

pub(crate) mod applications;

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
        args: Vec<&str>,
        env: HashMap<&str, &str>,
    ) -> TestProcess<'a> {
        let bin_path = get_mirrord_binary();
        let mut exec_args: Vec<&str> = vec!["exec", "-t", "pod/mock-target", "--", &executable];
        exec_args.extend(args);
        let child = Command::new(bin_path)
            .args(exec_args)
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
        self.env.insert("MIRRORD_REMOTE_DNS", "false");
    }

    fn with_custom_env(&mut self, custom_env: HashMap<&'a str, &'a str>) {
        self.env.extend(custom_env);
    }
}

fn get_mirrord_binary() -> String {
    let test_bin_path =
        build_test_binary("mirrord", "../../../").expect("error building test binary");

    test_bin_path
        .to_str()
        .expect("error converting test binary path to string")
        .to_string()
}

struct LayerConnection {
    codec: Framed<TcpStream, DaemonCodec>,
}

impl LayerConnection {
    async fn accept_library_connection(listener: &TcpListener) -> LayerConnection {
        let (stream, _) = listener.accept().await.unwrap();
        let codec = Framed::new(stream, DaemonCodec::new());
        LayerConnection { codec }
    }
}
