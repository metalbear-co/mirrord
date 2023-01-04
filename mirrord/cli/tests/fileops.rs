#[cfg(test)]
mod common;
mod fileops {
    use std::time::Duration;

    use rstest::rstest;

    //use crate::common::TestProcess;

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(60))]
    async fn test_pwrite() {
        todo!()
    }
}
