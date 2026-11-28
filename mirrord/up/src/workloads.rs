//! Where `mirrord up` finds the workloads it targets by a service's name: the cluster's own
//! workloads through the kubeconfig, or, under an operator login, the targets that operator lists
//! for the logged-in user.
//!
//! Only the operator's own API group answers an operator login, so the deployments, pods and
//! namespaces the kubeconfig path reads are out of reach there. The operator lists the same
//! workloads as `TargetCrd`s, already filtered to what the user may list.

use std::sync::Arc;

use futures::future::BoxFuture;
use mirrord_config::target::{Target, TargetType};

use crate::{UpError, list_namespaces, list_workloads};

/// Where [`crate::resolve_services`] looks up the workload of a service without a `target.path`.
#[derive(Clone)]
pub enum WorkloadSource {
    /// The kubeconfig: one client per kube context, reading the cluster's workloads.
    Kubeconfig,
    /// The operator an operator login reaches. Kube contexts mean nothing there, so they are not
    /// used.
    Operator(Arc<dyn OperatorTargets>),
}

/// The targets the operator of an operator login lists for the logged-in user. The CLI
/// implements it, since the operator's client and types live there.
pub trait OperatorTargets: Send + Sync {
    /// The namespace a session without `target.namespace` runs in, so a target looked up without
    /// one is looked up where its session will run.
    fn default_namespace(&self) -> &str;

    /// Every target the user may list in `namespace`.
    fn targets<'a>(&'a self, namespace: &'a str) -> BoxFuture<'a, Result<Vec<Target>, UpError>>;

    /// The namespaces the user may pick in the wizard, never empty.
    fn namespaces(&self) -> BoxFuture<'_, Vec<String>>;
}

/// The one cluster view a lookup or the wizard reads, picked from a [`WorkloadSource`].
#[derive(Clone, Copy)]
pub(crate) enum Lister<'a> {
    Kube(&'a kube::Client),
    Operator(&'a dyn OperatorTargets),
}

impl Lister<'_> {
    pub(crate) fn default_namespace(&self) -> &str {
        match self {
            Self::Kube(client) => client.default_namespace(),
            Self::Operator(targets) => targets.default_namespace(),
        }
    }

    /// Workload paths of the `kinds` in `namespace`, in the order of `kinds`.
    pub(crate) async fn workloads(
        &self,
        namespace: &str,
        kinds: &[TargetType],
    ) -> Result<Vec<String>, UpError> {
        match self {
            Self::Kube(client) => list_workloads(client, namespace, kinds).await,
            Self::Operator(targets) => Ok(paths_by_kind(&targets.targets(namespace).await?, kinds)),
        }
    }

    pub(crate) async fn namespaces(&self) -> Result<Vec<String>, UpError> {
        match self {
            Self::Kube(client) => list_namespaces(client).await,
            Self::Operator(targets) => Ok(targets.namespaces().await),
        }
    }
}

/// The paths of `targets` whose kind is one of `kinds`, grouped in the order of `kinds`, the way
/// the kubeconfig listing returns them. A lookup takes the first path with the service's name, so
/// this order is what makes a deployment win over a pod of the same name.
pub(crate) fn paths_by_kind(targets: &[Target], kinds: &[TargetType]) -> Vec<String> {
    kinds
        .iter()
        .flat_map(|kind| {
            targets
                .iter()
                .filter(move |target| TargetType::from(*target) == *kind)
                .map(ToString::to_string)
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use std::{path::PathBuf, str::FromStr, sync::Mutex};

    use futures::{FutureExt, executor::block_on};
    use mirrord_config::config::EnvKey;
    use tempfile::TempDir;

    use super::*;
    use crate::{INFERRED_TARGET_TYPES, load_up_config, resolve_services};

    fn target(path: &str) -> Target {
        Target::from_str(path).expect("a valid target path")
    }

    /// An operator that lists `targets` in every namespace, or refuses every listing when
    /// `forbidden`, and records which namespaces it was asked about.
    #[derive(Default)]
    struct FakeOperator {
        targets: Vec<Target>,
        forbidden: bool,
        asked: Mutex<Vec<String>>,
    }

    impl OperatorTargets for FakeOperator {
        fn default_namespace(&self) -> &str {
            "default"
        }

        fn targets<'a>(
            &'a self,
            namespace: &'a str,
        ) -> BoxFuture<'a, Result<Vec<Target>, UpError>> {
            self.asked
                .lock()
                .expect("not poisoned")
                .push(namespace.to_owned());
            let listing = if self.forbidden {
                Err(UpError::OperatorTargetsForbidden {
                    host: "mirrord.corp.example.com".to_owned(),
                    namespace: namespace.to_owned(),
                })
            } else {
                Ok(self.targets.clone())
            };
            futures::future::ready(listing).boxed()
        }

        fn namespaces(&self) -> BoxFuture<'_, Vec<String>> {
            futures::future::ready(vec!["default".to_owned()]).boxed()
        }
    }

    fn up_file(yaml: &str) -> (TempDir, PathBuf) {
        let directory = TempDir::new().expect("create a temp dir");
        let path = directory.path().join("mirrord-up.yaml");
        std::fs::write(&path, yaml).expect("write the up file");
        (directory, path)
    }

    fn resolve(
        yaml: &str,
        operator: Arc<FakeOperator>,
    ) -> Result<Vec<crate::SubprocessCfg>, UpError> {
        let (_directory, path) = up_file(yaml);
        let key = EnvKey::Provided("key".to_owned());
        let config = load_up_config(&path, &key).expect("load the up file");
        block_on(resolve_services(
            config,
            &path,
            None,
            &key,
            false,
            &WorkloadSource::Operator(operator),
        ))
    }

    /// A lookup takes the first path named after the service, so the operator's listing is
    /// reordered by kind the way the kubeconfig listing comes: a deployment wins over a
    /// statefulset, a rollout and a pod of the same name, whatever order the operator lists them
    /// in.
    #[test]
    fn operator_targets_are_ordered_by_inferred_kind() {
        let listed = [
            target("pod/api"),
            target("rollout/api"),
            target("cronjob/api"),
            target("statefulset/api"),
            target("deployment/api"),
        ];

        assert_eq!(
            paths_by_kind(&listed, &INFERRED_TARGET_TYPES),
            [
                "deployment/api",
                "statefulset/api",
                "rollout/api",
                "pod/api"
            ]
        );
    }

    /// Under an operator login, a service without `target.path` gets the workload the operator
    /// lists under its name, looked up in the namespace its session runs in, and no kubeconfig is
    /// read: the fake is the only source there is.
    #[test]
    fn a_service_without_a_target_gets_the_operator_listed_workload() {
        let operator = Arc::new(FakeOperator {
            targets: vec![target("pod/api-7d9f"), target("deployment/api")],
            ..Default::default()
        });

        let services = resolve(
            r#"
            services:
              api:
                run: { command: [echo] }
              worker:
                target: { path: deployment/worker }
                run: { command: [echo] }
            "#,
            operator.clone(),
        )
        .expect("resolve the services");

        let api = services
            .iter()
            .find(|service| &*service.service_name == "api")
            .expect("the api service");
        assert_eq!(api.config.target.path, Some(target("deployment/api")));
        assert_eq!(api.config.target.namespace, None);
        // Only the service without a target was looked up, in the session's namespace.
        assert_eq!(*operator.asked.lock().expect("not poisoned"), ["default"]);
    }

    /// Without a terminal, a service the operator lists nothing for fails at once, naming the
    /// namespace it was looked up in, as it does through the kubeconfig.
    #[test]
    fn a_missing_target_without_a_terminal_still_fails() {
        let operator = Arc::new(FakeOperator {
            targets: vec![target("deployment/other")],
            ..Default::default()
        });

        let Err(error) = resolve(
            r#"
            services:
              api:
                run: { command: [echo] }
            "#,
            operator,
        ) else {
            panic!("nothing is named api, so the lookup fails");
        };

        assert!(
            matches!(
                &error,
                UpError::NoInferredTarget { service, namespace }
                    if &**service == "api" && namespace == "default"
            ),
            "{error:?}"
        );
    }

    /// Without a terminal, as an IDE runs it, a namespace the login may not list targets in
    /// fails the run with the permission to ask for, not as a missing workload, and the terminal's
    /// skipping never applies.
    #[test]
    fn a_forbidden_listing_without_a_terminal_names_the_permission() {
        let operator = Arc::new(FakeOperator {
            forbidden: true,
            ..Default::default()
        });

        let Err(error) = resolve(
            r#"
            services:
              api:
                run: { command: [echo] }
            "#,
            operator,
        ) else {
            panic!("the listing is forbidden, so the lookup fails");
        };

        assert!(
            matches!(&error, UpError::OperatorTargetsForbidden { namespace, .. } if namespace == "default"),
            "{error:?}"
        );
    }

    /// The decision the terminal lookup and the wizard both use (their prompts need a terminal,
    /// so this tests the decision alone): a forbidden namespace is skipped when another can be
    /// picked, and stays the error naming the permission when it is the only one. Any other
    /// failure stops the run either way.
    #[test]
    fn a_forbidden_namespace_is_skipped_only_when_another_can_be_picked() {
        let forbidden = || {
            Err(UpError::OperatorTargetsForbidden {
                host: "mirrord.corp.example.com".to_owned(),
                namespace: "payments".to_owned(),
            })
        };
        assert_eq!(
            crate::skip_forbidden_namespace(forbidden(), true).unwrap(),
            None
        );
        assert!(matches!(
            crate::skip_forbidden_namespace(forbidden(), false),
            Err(UpError::OperatorTargetsForbidden { .. })
        ));

        let listed = Ok(vec!["deployment/api".to_owned()]);
        assert_eq!(
            crate::skip_forbidden_namespace(listed, true).unwrap(),
            Some(vec!["deployment/api".to_owned()])
        );

        let other = Err(UpError::NoWorkloads("payments".to_owned()));
        assert!(crate::skip_forbidden_namespace(other, true).is_err());
    }
}
