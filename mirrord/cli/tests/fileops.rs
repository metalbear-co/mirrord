#[cfg(test)]
mod common;
mod fileops {
    use std::time::Duration;

    use rstest::rstest;

    //use crate::common::TestProcess;
    use crate::common::applications::Application;

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    async fn test_pwrite(#[values(Application::RustFileOps)] application: Application) {
        let executable = application.get_executable().await;
        todo!()
    }
}
