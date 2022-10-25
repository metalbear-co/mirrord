use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use itertools::Itertools;
use rand::distributions::{Alphanumeric, DistString};
use tracing_appender::{
    non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard},
    WorkerOptions,
};

use crate::detour::{detour_bypass_off, detour_bypass_on};

pub(crate) static TRACING_GUARDS: Mutex<Vec<WorkerGuard>> = Mutex::new(vec![]);
pub(crate) static LOG_FILE_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn file_tracing_writer() -> NonBlocking {
    let run_id = std::env::var("MIRRORD_RUN_ID").unwrap_or_else(|_| {
        let run_id = Alphanumeric
            .sample_string(&mut rand::thread_rng(), 10)
            .to_lowercase();

        std::env::set_var("MIRRORD_RUN_ID", run_id.clone());

        run_id
    });

    let log_file_name = format!("{}-mirrord-layer.log", run_id);

    let log_path = LOG_FILE_PATH.get_or_init(|| PathBuf::from("/tmp/mirrord").join(log_file_name));

    let file_appender = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(true)
        .open(log_path)
        .unwrap();

    let (non_blocking, guard) = NonBlockingBuilder::default()
        .worker_options(
            WorkerOptions::default()
                .on_thread_start(detour_bypass_on)
                .on_thread_stop(detour_bypass_off),
        )
        .finish(file_appender);

    let _ = TRACING_GUARDS.lock().map(|mut guards| guards.push(guard));

    non_blocking
}

pub fn print_support_message() {
    if let Some(log_file) = LOG_FILE_PATH.get() {
        let issue_link = create_github_link(log_file);

        println!("mirrord encountered an error, please create an issue over at github, It would be much appreciated: {}", issue_link);
    }
}

fn create_github_link<P: AsRef<Path>>(log_file: P) -> String {
    let binary_type = std::env::args().join(" ");
    let os_version = format!("{}", os_info::get());
    let base_url = format!("https://github.com/metalbear-co/mirrord/issues/new?assignees=&labels=bug&template=bug_report.yml&binary_type={}&os_version={}", urlencoding::encode(&binary_type), urlencoding::encode(&os_version));

    if let Ok(logs) = fs::read(log_file) {
        let encoded_logs = urlencoding::encode_binary(&logs);

        if encoded_logs.len() > 6000 {
            format!(
                "{}&logs={}",
                base_url,
                &encoded_logs[(encoded_logs.len() - 6000)..]
            )
        } else {
            format!("{}&logs={}", base_url, encoded_logs)
        }
    } else {
        format!("{}", base_url)
    }
}
