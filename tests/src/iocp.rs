//! Windows-only e2e test that exercises the layer's IO completion port
//! (IOCP) plumbing via a real-world async file IO client running through
//! `mirrord exec` against a `basic_service` pod.
//!
//! The test launches a `Windows-only` binary that reads `C:\app\test.txt`
//! (which `mirrord-layer-win` resolves to `/app/test.txt` -- a 446-byte
//! Lorem Ipsum file shipped by the `mirrord-pytest:latest` image, cited at
//! `tests/python-e2e/files_ro.py:7` and `tests/src/utils.rs:27`) and
//! asserts on its content. The binary exits non-zero on failure; the
//! helper `iocp_tests::run_iocp_app` asserts on the exit code.
//!
//! - `asynctext_csharp`: small .NET app that reads the file via `File.ReadAllTextAsync` -- the
//!   runtime's overlapped+IOCP code path -- and `memcmp`s against the expected Lorem Ipsum.
#[cfg(test)]
mod iocp_tests {
    use std::time::Duration;

    use rstest::*;

    use crate::utils::{
        application::Application, kube_service::KubeService, services::basic_service,
    };

    /// Default 30s timeout for every IOCP e2e test. The C# binary is a
    /// single read. Bump only if a new test legitimately needs more
    /// wallclock.
    const IOCP_TIMEOUT: Duration = Duration::from_secs(30);

    /// Shared launcher: open the file mode override so the layer remotes
    /// `/app/test.txt`, `mirrord exec` the test binary, assert exit code 0.
    /// Each test that hangs surfaces via the per-test rstest timeout, not
    /// here.
    async fn run_iocp_app(app: Application, service: KubeService) {
        // Force the layer to treat /app/test.txt as a remote read regardless
        // of the platform default (MIRRORD_FILE_MODE=local on Windows per
        // `mirrord/layer-tests/tests/common/mod.rs:718`).
        let env = vec![
            ("MIRRORD_FILE_MODE", "localwithoverrides"),
            ("MIRRORD_FILE_READ_ONLY_PATTERN", "/app/test.txt"),
        ];
        let mut process = app
            .run(
                &service.pod_container_target(),
                Some(&service.namespace),
                None,
                Some(env),
            )
            .await;
        let res = process.wait().await;
        assert!(
            res.success(),
            "{app:?} exited with {res:?}: see stdout above for the failure \
             (the binary prints a `FAIL ...` line before exiting non-zero)",
        );
    }

    /// C# binary at `tests/cs-e2e/AsyncText/`. Exercises the layer through
    /// .NET's `File.ReadAllTextAsync`, which on Windows goes through
    /// `FileStream` + overlapped IO + an IOCP managed by the runtime
    /// thread pool. The exe `memcmp`s the read content against a vendored
    /// copy of the expected Lorem Ipsum and exits non-zero on mismatch.
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[trace]
    #[tokio::test]
    #[timeout(IOCP_TIMEOUT)]
    pub async fn asynctext_csharp(
        #[future]
        #[notrace]
        basic_service: KubeService,
    ) {
        run_iocp_app(Application::AsyncTextCsharp, basic_service.await).await;
    }
}
