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

    /// Shared launcher: open the file mode override so the layer remotes
    /// `/app/test.txt`, `mirrord exec` the test binary, assert exit code 0.
    /// Each test that hangs surfaces via the per-test rstest timeout, not
    /// here.
    async fn run_iocp_app(app: Application, service: KubeService) {
        // Force the layer to treat /app/test.txt as a remote read regardless
        // of the platform default (MIRRORD_FILE_MODE=local on Windows per
        // `mirrord/layer-tests/tests/common/mod.rs:718`).
        //
        // Keep DNS and outgoing traffic local: the C# app runs from source via
        // `dotnet run`, whose build-time NuGet restore must reach the real
        // internet. Routing it through the pod fails restore with `NU1301:
        // Unable to load the service index for source api.nuget.org`. Only the
        // `/app/test.txt` read needs to be remote; everything else stays local.
        let env = vec![
            ("MIRRORD_FILE_MODE", "localwithoverrides"),
            ("MIRRORD_FILE_READ_ONLY_PATTERN", "/app/test.txt"),
            ("MIRRORD_REMOTE_DNS", "false"),
            ("MIRRORD_TCP_OUTGOING", "false"),
            ("MIRRORD_UDP_OUTGOING", "false"),
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
    // 120s: the actual file IO is a single read, but the C# app runs from
    // source via `dotnet run`, so the first invocation pays for a cold
    // restore + build before the read happens -- 30s wasn't enough.
    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[trace]
    #[tokio::test]
    #[timeout(Duration::from_secs(120))]
    pub async fn asynctext_csharp(
        #[future]
        #[notrace]
        basic_service: KubeService,
    ) {
        run_iocp_app(Application::AsyncTextCsharp, basic_service.await).await;
    }
}
