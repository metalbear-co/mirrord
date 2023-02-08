#[cfg(test)]
/// Tests for the `mirrord ls` command
mod target {

    use regex::Regex;
    use rstest::rstest;

    use crate::utils::{run_ls, service, KubeService};

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_mirrord_ls(#[future] service: KubeService) {
        let service = service.await;
        let mut process = run_ls(None, None);
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        let stdout = process.get_stdout();
        let targets: Vec<String> = serde_json::from_str(&stdout).unwrap();
        let re = Regex::new(r"^pod/.+(/container/.+)?$").unwrap();
        targets
            .iter()
            .for_each(|output| assert!(re.is_match(output)));
        assert!(targets
            .iter()
            .any(|output| output.starts_with(&format!("pod/{}", service.name))));
    }
}
