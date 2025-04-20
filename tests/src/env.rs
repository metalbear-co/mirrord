#![cfg(test)]

mod env_tests {
    use std::time::Duration;

    use rstest::*;

    use crate::utils::{
        application::env::EnvApp, kube_service::KubeService, run_command::run_exec_with_target,
        services::basic_service,
    };

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn bash_remote_env_vars(
        #[future] basic_service: KubeService,
        #[values(EnvApp::BashInclude, EnvApp::BashExclude, EnvApp::Bash)] application: EnvApp,
    ) {
        remote_env_vars_works(basic_service, application).await;
    }

    #[cfg_attr(not(feature = "job"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    pub async fn remote_env_vars_works(
        #[future] basic_service: KubeService,
        #[values(
            EnvApp::Go21,
            EnvApp::Go22,
            EnvApp::Go23,
            EnvApp::NodeInclude,
            EnvApp::NodeExclude
        )]
        application: EnvApp,
    ) {
        let service = basic_service.await;
        let mut process = run_exec_with_target(
            application.command(),
            &service.pod_container_target(),
            None,
            application.mirrord_args(),
            None,
        )
        .await;
        let res = process.wait().await;
        assert!(res.success());
    }
}
