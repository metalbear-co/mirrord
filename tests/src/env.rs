#[cfg(test)]

mod env {
    use std::time::Duration;

    use rstest::*;

    use crate::utils::{run_exec_with_target, service, EnvApp, KubeService};

    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(240))]
    pub async fn bash_remote_env_vars(
        #[future] service: KubeService,
        #[values(EnvApp::BashInclude, EnvApp::BashExclude, EnvApp::Bash)] application: EnvApp,
    ) {
        remote_env_vars_works(service, application).await;
    }

    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(120))]
    pub async fn remote_env_vars_works(
        #[future] service: KubeService,
        #[values(
            EnvApp::Go18,
            EnvApp::Go19,
            EnvApp::Go20,
            EnvApp::NodeInclude,
            EnvApp::NodeExclude
        )]
        application: EnvApp,
    ) {
        let service = service.await;
        let mut process = run_exec_with_target(
            application.command(),
            &service.target,
            None,
            application.mirrord_args(),
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
    }
}
