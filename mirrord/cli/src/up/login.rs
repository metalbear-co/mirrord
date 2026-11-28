//! `mirrord up` under an operator login (`mirrord login --url`): the services' workloads are looked
//! up through that operator, and the settings the login makes meaningless are dealt with once,
//! for the whole run, before any service starts.
//!
//! Each service's session reads the login itself, the way `mirrord exec` does, so nothing here
//! hands the login to the services.

use std::{collections::BTreeSet, sync::Arc};

use futures::future::BoxFuture;
use kube::{Api, api::ListParams};
use mirrord_config::target::Target;
use mirrord_operator::{
    client::{OperatorAccess, check_config_under_login, check_operator_enabled_under_login},
    crd::TargetCrd,
    sso::OperatorLogin,
};
use mirrord_up::{OperatorTargets, SubprocessCfg, UpConfig, UpError, WorkloadSource};

use super::UpCliError;
use crate::{
    error::CliError,
    kube::kube_client_and_access_from_layer_config,
    list::{known_targets, namespaces_through_operator_route},
};

/// The targets the operator of the login lists for the logged-in user, standing in for the
/// cluster's workloads, which the login cannot read.
struct LoginTargets {
    client: kube::Client,
    host: String,
}

impl OperatorTargets for LoginTargets {
    fn default_namespace(&self) -> &str {
        self.client.default_namespace()
    }

    fn targets<'a>(&'a self, namespace: &'a str) -> BoxFuture<'a, Result<Vec<Target>, UpError>> {
        Box::pin(async move {
            match Api::<TargetCrd>::namespaced(self.client.clone(), namespace)
                .list(&ListParams::default())
                .await
            {
                Ok(list) => Ok(known_targets(list.items).collect()),
                Err(kube::Error::Api(status)) if status.code == 403 => {
                    Err(UpError::OperatorTargetsForbidden {
                        host: self.host.clone(),
                        namespace: namespace.to_owned(),
                    })
                }
                Err(source) => Err(UpError::OperatorTargets {
                    host: self.host.clone(),
                    namespace: namespace.to_owned(),
                    source: Box::new(source),
                }),
            }
        })
    }

    fn namespaces(&self) -> BoxFuture<'_, Vec<String>> {
        Box::pin(namespaces_through_operator_route(
            &self.client,
            self.client.default_namespace(),
        ))
    }
}

/// Where `mirrord up` looks up the services' workloads under `login`: the targets its operator
/// lists, through a client built from the up file's common settings.
///
/// `None` when the login is gone by the time the client is built (a `mirrord logout` in
/// between): the run then goes on through the kubeconfig, as its services will.
pub(super) async fn workload_source(
    up_config: &UpConfig,
    login: &OperatorLogin,
) -> Result<Option<WorkloadSource>, UpCliError> {
    let config = up_config
        .common_layer_config()
        .map_err(UpError::Validation)?;
    let (client, access) = kube_client_and_access_from_layer_config(&config)
        .await
        .map_err(|error| UpCliError::Login(Box::new(error)))?;
    let OperatorAccess::Login(..) = access else {
        return Ok(None);
    };
    Ok(Some(WorkloadSource::Operator(Arc::new(LoginTargets {
        client,
        host: login.host().to_owned(),
    }))))
}

/// Makes every service's config fit `login` before any session starts, and returns the warnings
/// to print once for the whole run.
///
/// A service whose config turns the operator off (`operator: false` in its `config_patch`, or the
/// global default `global_operator`) stops the run, named: its session would refuse to start under
/// the login after the others had started. `kubeconfig` and `kube_context` mean nothing under a
/// login, so they are cleared, and the sessions don't each warn about them again.
///
/// The operator is required from here on, as each session requires it under the login, so the
/// checks before launch fail on an operator that can't be used instead of passing the service
/// on without it.
pub(super) fn fit_services_to_login(
    services: &mut [SubprocessCfg],
    login: &OperatorLogin,
    global_operator: Option<bool>,
) -> Result<BTreeSet<String>, UpCliError> {
    let mut warnings = BTreeSet::new();
    for service in services.iter_mut() {
        let service_error = |error| UpCliError::ServiceUnderLogin {
            service: service.service_name.clone(),
            error: Box::new(CliError::from(error)),
        };

        let mut merged = service.config.clone();
        merged.operator = merged.operator.or(global_operator);
        check_operator_enabled_under_login(&merged, login).map_err(service_error)?;

        warnings.extend(check_config_under_login(&service.config, login).map_err(service_error)?);
        service.config.kube_context = None;
        service.config.kubeconfig = None;
        service.config.operator = Some(true);
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, Utc};
    use mirrord_config::config::EnvKey;
    use mirrord_operator::sso::OperatorLoginToken;

    use super::*;

    fn login() -> OperatorLogin {
        OperatorLogin {
            url: "https://mirrord.corp.example.com".to_owned(),
            token: OperatorLoginToken::from("secret-token".to_owned()),
            username: "dev@corp.example.com".to_owned(),
            account: "dev@corp.example.com".to_owned(),
            expires_at: Utc::now() + TimeDelta::hours(1),
        }
    }

    /// The services of the up file `yaml` as `mirrord up` assembles them. Every target is named,
    /// so nothing is looked up in a cluster.
    async fn assemble(yaml: &str) -> Vec<SubprocessCfg> {
        let directory = tempfile::tempdir().expect("create a temp dir");
        let path = directory.path().join("mirrord-up.yaml");
        std::fs::write(&path, yaml).expect("write the up file");
        let key = EnvKey::Provided("key".to_owned());
        let config = mirrord_up::load_up_config(&path, &key).expect("load the up file");
        mirrord_up::resolve_services(
            config,
            &path,
            None,
            &key,
            false,
            &WorkloadSource::Kubeconfig,
        )
        .await
        .expect("assemble the services")
    }

    /// Before, every service's session printed its own `kube_context is ignored` line, once per
    /// service: `mirrord up` copied the context into each config. Now the run prints each warning
    /// once, the sessions get no context or kubeconfig to warn about, and the operator is required
    /// so the checks before launch don't pass a service on without it.
    #[tokio::test]
    async fn the_context_warning_is_returned_once_and_the_contexts_are_cleared() {
        let mut services = assemble(
            r#"
            common:
              context: staging
            services:
              api:
                target: { path: deployment/api }
                run: { command: [echo] }
              worker:
                target: { path: deployment/worker }
                run: { command: [echo] }
                config_patch:
                  kubeconfig: /home/dev/.kube/other
            "#,
        )
        .await;

        let warnings = fit_services_to_login(&mut services, &login(), None).unwrap();

        assert_eq!(
            warnings.into_iter().collect::<Vec<_>>(),
            [
                "kube_context is ignored while logged in to mirrord.corp.example.com",
                "kubeconfig is ignored while logged in to mirrord.corp.example.com",
            ]
        );
        for service in &services {
            assert_eq!(
                service.config.kube_context, None,
                "{}",
                service.service_name
            );
            assert_eq!(service.config.kubeconfig, None, "{}", service.service_name);
            assert_eq!(
                service.config.operator,
                Some(true),
                "{}",
                service.service_name
            );
        }
    }

    /// `operator: false` on one service stops the run naming that service, before any session
    /// starts: its session would refuse the login after the others were already running.
    #[tokio::test]
    async fn operator_false_on_one_service_stops_the_run_naming_it() {
        let mut services = assemble(
            r#"
            services:
              api:
                target: { path: deployment/api }
                run: { command: [echo] }
              worker:
                target: { path: deployment/worker }
                run: { command: [echo] }
                config_patch:
                  operator: false
            "#,
        )
        .await;

        let error = fit_services_to_login(&mut services, &login(), None).unwrap_err();

        assert_eq!(
            error.to_string(),
            "worker: operator: false can't be used while logged in to mirrord.corp.example.com. \
             Run mirrord logout --url https://mirrord.corp.example.com first."
        );
    }

    /// A global `operator: false` is merged in the session after its config is resolved, so the
    /// run checks the merged value too.
    #[tokio::test]
    async fn a_global_operator_false_stops_the_run() {
        let mut services = assemble(
            r#"
            services:
              api:
                target: { path: deployment/api }
                run: { command: [echo] }
            "#,
        )
        .await;

        let error = fit_services_to_login(&mut services, &login(), Some(false)).unwrap_err();
        assert!(
            error.to_string().starts_with("api: operator: false"),
            "{error}"
        );

        let mut services = assemble(
            r#"
            services:
              api:
                target: { path: deployment/api }
                run: { command: [echo] }
                config_patch:
                  operator: true
            "#,
        )
        .await;
        assert!(fit_services_to_login(&mut services, &login(), Some(false)).is_ok());
    }
}
