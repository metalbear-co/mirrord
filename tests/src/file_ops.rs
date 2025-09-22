#![allow(dead_code, unused)]
#[cfg(test)]
mod file_ops_tests {

    use std::{
        fs::{create_dir, remove_dir, remove_dir_all},
        io::Write,
        path::Path,
        time::Duration,
    };

    use k8s_openapi::api::core::v1::Pod;
    use kube::{api::LogParams, Api, Client};
    use rstest::*;
    use serde::Deserialize;
    use tempfile::NamedTempFile;

    use crate::utils::{
        application::{file_ops::FileOps, GoVersion},
        kube_client,
        kube_service::KubeService,
        run_command::run_exec_with_target,
        services::{basic_service, go_statfs_service},
    };

    #[cfg_attr(target_os = "windows", ignore)]
    #[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn file_ops(
        #[future]
        #[notrace]
        basic_service: KubeService,
        #[values(FileOps::Python, FileOps::Rust)] ops: FileOps,
    ) {
        let service = basic_service.await;
        let command = ops.command();

        let mut args = vec!["--fs-mode", "read"];
        if cfg!(feature = "ephemeral") {
            args.extend(["-e"].into_iter());
        }

        let env = vec![("MIRRORD_FILE_READ_WRITE_PATTERN", "/tmp.*")];
        let mut process = run_exec_with_target(
            command,
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(args),
            Some(env),
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());
        ops.assert(process).await;
    }

    //#[timeout(Duration::from_secs(240))]
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn file_ops_ro(
        #[future]
        #[notrace]
        basic_service: KubeService,
    ) {
        // NOTE(gabriela): Windows loves to use it's "App execution alias" functionality
        // for python binaries
        // ```
        // PS C:\dev\rust\mirrord\tests> $(Get-Command python3).path
        // C:\Users\gabriela\AppData\Local\Microsoft\WindowsApps\python3.exe
        // ```
        // So this should be overrideable in this context.
        let python_command = std::env::var("MIRRORD_PYTHON_FILE").unwrap_or("python3".to_string());

        let service = basic_service.await;
        let python_command = [
            python_command.as_str(),
            "-B",
            "-m",
            "unittest",
            "-f",
            "python-e2e/files_ro.py",
        ]
        .map(String::from)
        .to_vec();

        let mut process = run_exec_with_target(
            python_command,
            &service.pod_container_target(),
            Some(&service.namespace),
            None,
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());
        process.assert_python_fileops_stderr().await;
    }

    #[cfg_attr(target_os = "windows", ignore)]
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn file_ops_unlink(
        #[future]
        #[notrace]
        basic_service: KubeService,
    ) {
        let service = basic_service.await;
        let python_command = [
            "python3",
            "-B",
            "-m",
            "unittest",
            "-f",
            "python-e2e/files_unlink.py",
        ]
        .map(String::from)
        .to_vec();

        // use mirrord config file to specify remote and local directories, as well as mapping
        let config = serde_json::json!({
            "feature": {
                "fs": {
                    "mode": "localwithoverrides",
                    "read_write": ".*remote_test.*",
                    "mapping": {
                        "source_test": "sink_test"
                    }
                }
            }
        })
        .to_string();
        let mut config_file = NamedTempFile::with_suffix(".json").unwrap();
        config_file.write_all(config.as_bytes()).unwrap();
        let file_name = config_file.path().to_string_lossy();

        let mut args = vec!["-f", file_name.as_ref()];

        let mut process = run_exec_with_target(
            python_command,
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(args),
            None,
        )
        .await;
        let res = process.wait().await;

        assert!(res.success());
        process.assert_python_fileops_stderr().await;
    }

    // Currently fails due to Layer >> AddressConversion in ci for some reason
    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn bash_file_exists(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let bash_command = ["bash", "bash-e2e/file.sh", "exists"]
            .map(String::from)
            .to_vec();
        let mut process = run_exec_with_target(
            bash_command,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    // currently there is an issue with piping across forks of processes so 'test_bash_file_read'
    // and 'test_bash_file_write' cannot pass

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn bash_file_read(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let bash_command = ["bash", "bash-e2e/file.sh", "read"]
            .map(String::from)
            .to_vec();
        let mut process = run_exec_with_target(
            bash_command,
            &service.pod_container_target(),
            None,
            None,
            None,
        )
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn bash_file_write(#[future] basic_service: KubeService) {
        let service = basic_service.await;
        let bash_command = ["bash", "bash-e2e/file.sh", "write"]
            .map(String::from)
            .to_vec();
        let args = vec!["--rw"];
        let mut process = run_exec_with_target(
            bash_command,
            &service.pod_container_target(),
            None,
            Some(args),
            None,
        )
        .await;

        let res = process.wait().await;
        assert!(res.success());
    }

    /// Test our getdents64 Go syscall hook, for `os.ReadDir` on go, and mkdir and rmdir.
    /// This is an E2E test and not an integration test in order to test the agent side of the
    /// detours.
    #[cfg_attr(target_os = "windows", ignore)]
    #[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn go_dir(
        #[future]
        #[notrace]
        basic_service: KubeService,
        #[values(GoVersion::GO_1_23, GoVersion::GO_1_24, GoVersion::GO_1_25)] go_version: GoVersion,
    ) {
        let service = basic_service.await;
        let command = FileOps::GoDir(go_version).command();

        let mut args = Vec::new();

        if cfg!(feature = "ephemeral") {
            args.extend(["-e"].into_iter());
        }

        let mut process = run_exec_with_target(
            command,
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(args),
            Some(vec![(
                "MIRRORD_FILE_READ_WRITE_PATTERN",
                "^/app/test_mkdir$",
            )]),
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());
    }

    #[derive(Deserialize, Debug)]
    struct GoStatfs {
        bavail: u64,
        bfree: u64,
        blocks: u64,
        bsize: i64,
        ffree: u64,
        files: u64,
        flags: i64,
        frsize: i64,
        fsid: [i32; 2],
        namelen: i64,
        spare: [i64; 4],
        r#type: i64,
    }

    impl PartialEq for GoStatfs {
        fn eq(&self, other: &Self) -> bool {
            // bavail and bfree  and ffree change constantly, so they will usually not be the same
            // in the two calls, so we can't really reliably test those fields..
            self.blocks == other.blocks
                && self.bsize == other.bsize
                && self.files == other.files
                && self.flags == other.flags
                && self.frsize == other.frsize
                // that field is crazy, idk
                // && self.fsid == other.fsid
                && self.namelen == other.namelen
                && self.spare == other.spare
                && self.r#type == other.r#type
        }
    }

    /// Test that after going through all the conversions between the agent and the user program,
    /// the statfs values are correct.
    /// This is to prevent a regression to a bug we had where because of `statfs`/`statfs64`
    /// struct conversions, we were returning an invalid struct to go when it called SYS_statfs.
    #[cfg_attr(target_os = "windows", ignore)]
    #[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn go_statfs(
        #[future] go_statfs_service: KubeService,
        #[future] kube_client: Client,
    ) {
        let app = FileOps::GoStatfs(GoVersion::GO_1_25);
        let service = go_statfs_service.await;
        let client = kube_client.await;
        let command = app.command();

        let mut args = vec!["--fs-mode", "read"];

        if cfg!(feature = "ephemeral") {
            args.extend(["-e"].into_iter());
        }

        let mut process = run_exec_with_target(
            command,
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(args),
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());
        let mirrord_statfs_output = process.get_stdout().await;
        println!("statfs via mirrord:\n{}", mirrord_statfs_output);
        let statfs_from_mirrord: GoStatfs = serde_json::from_str(&mirrord_statfs_output).unwrap();

        let pod_api = Api::<Pod>::namespaced(client, &service.namespace);
        let statfs_from_pod: GoStatfs = loop {
            let logs = pod_api
                .logs(&service.pod_name, &LogParams::default())
                .await
                .unwrap();
            println!("{}", logs);
            match serde_json::from_str(&logs) {
                Ok(statfs) => break statfs,
                Err(err) => println!(
                    "Could not deserialize statfs from logs. Got error: {:#?}",
                    err
                ),
            }
            // It's possible we didn't get all the logs yet, so the json is not valid.
            // Wait a bit and fetch the logs again.
            tokio::time::sleep(Duration::from_secs(3)).await;
        };
        assert_eq!(statfs_from_mirrord, statfs_from_pod);
    }
}
