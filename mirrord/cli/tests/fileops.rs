#[cfg(test)]
mod common;
mod fileops {
    use std::time::Duration;

    use rstest::rstest;

    use crate::common::{applications::Application, EnvProvider, LayerConnection, TestProcess};

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    async fn test_pwrite(#[values(Application::RustFileOps)] application: Application) {
        let executable = application.build_executable().get_executable();

        let mut test_process = TestProcess::new();
        test_process.with_basic_env();

        let LayerConnection { codec, addr } = LayerConnection::new().await;

        test_process.connect(&addr);
        todo!()
    }
}
