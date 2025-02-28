//! If you're looking into this `mod`, let it be known that you've made some poor choices
//! in your life that led you here.
//!
//! Has most of the code for modifying a user's [`docker_compose_types::Service`].
//!
//! These modifications are finnicky and _minefieldy_, so instead of free functions we have some
//! helper structs and traits here that try to make changing of fields really explicit.
use std::collections::HashMap;

use super::{steps::New, MIRRORD_COMPOSE_SIDECAR_SERVICE};

/// Wrapper for `env_vars` and `volumes` that we're adding to a compose service.
#[derive(Debug, Default, Clone)]
pub(super) struct ServiceInfo {
    /// Holds vars that we're adding to a service.
    pub(super) env_vars: HashMap<String, String>,
    /// Currently, only the `mirrord-sidecar` gets these, we have no volumes to add to user's
    /// services.
    pub(super) volumes: HashMap<String, String>,
}

/// Modifier of [`docker_compose_types::Service`] for user's services.
///
/// To avoid potential conflicts, or reseting fields before they're used, this struct follows a
/// series of steps that should make modifying these fields cleaner.
pub(super) struct ServiceComposer<'a, Step> {
    /// The step we're currently on.
    ///
    /// Used to lock the implementation of certain functions on the type-system level, hopefully
    /// avoiding potential misteps (pun intended) when modifying these finnicky fields.
    step: Step,

    /// The user's [`docker_compose_types::Service`] we're modifying.
    service: &'a mut docker_compose_types::Service,
}

/// Step where we reset the [`docker_compose_types::Service`] `ports`, and return the originals to
/// the caller, so it can be inserted in the `ports` section of the `mirrord-sidecar` service.
pub(super) struct ResetConflicts {
    /// The original user's service ports.
    ports: docker_compose_types::Ports,
}

impl<'a> ServiceComposer<'a, New> {
    /// Changes the [`docker_compose_types::Service`] `environment`, `depends_on`.
    ///
    /// - Next step: The original `ports` are left unmodified (if there were any), and are passed
    ///   on.
    pub(super) fn modify(
        service: &'a mut docker_compose_types::Service,
        service_info: &ServiceInfo,
    ) -> ServiceComposer<'a, ResetConflicts> {
        use docker_compose_types::Ports;

        service.modify_environment(service_info);
        service.modify_depends_on();

        service
            .volumes_from
            .push(MIRRORD_COMPOSE_SIDECAR_SERVICE.into());
        service.network_mode = Some(format!("service:{MIRRORD_COMPOSE_SIDECAR_SERVICE}"));

        // Thanks to `compose config` syntax conversion, this should either always be `Ports::Long`
        // if there are ANY ports, or `Ports::Short` if there are NO ports, so we take advantage
        // of this knwoledge to convert from `Ports::Short` to `Ports::Long` here as a
        // safeguard.
        if matches!(service.ports, Ports::Short(_)) {
            service.ports = Ports::Long(Default::default());
        }

        ServiceComposer {
            step: ResetConflicts {
                ports: service.ports.clone(),
            },
            service,
        }
    }
}

impl ServiceComposer<'_, ResetConflicts> {
    /// **Warning**: Minefield of options that are deserialized in a wonky manner, or must
    /// be reset due to conflicts with other options that we set to run the services with
    /// mirrord-sidecar.
    ///
    /// - `ports`: Conflicts with `network_mode: mirrord-sidecar`;
    /// - `networks`: Conflicts with `network_mode: mirrord-sidecar`;
    /// - `bind.propagation`: Has to be set, but `docker_compose_types` sets it to `null`, which is
    ///   not a thing;
    /// - `bind.selix`: Similar situation as `bind.propagation`;
    ///
    /// Returns the [`docker_compose_types::Ports`] from the user's service, before they were
    /// reset. These must be added to the `mirrord-sidecar`
    #[must_use]
    pub(super) fn reset_conflicts(self) -> docker_compose_types::Ports {
        let Self {
            service,
            step: ResetConflicts { ports },
        } = self;

        service.ports = Default::default();
        service.networks = Default::default();

        for volume in service.volumes.iter_mut() {
            if let docker_compose_types::Volumes::Advanced(advanced_volumes) = volume {
                if let Some(bind) = advanced_volumes.bind.as_mut() {
                    // Default value for docker.
                    bind.propagation.get_or_insert("rprivate".into());

                    // Seems like an acceptable default.
                    bind.selinux.get_or_insert("Z".into());
                }
            }
        }

        ports
    }
}

/// Helper trait implemented by [`docker_compose_types::Service`] so we can easily modify the user's
/// services, since these might require condition checks before a `match`.
trait ServiceExt {
    /// Changes this [`docker_compose_types::Service`] `environment` section, adding our env vars
    /// from [`ServiceInfo`].
    fn modify_environment(&mut self, service_info: &ServiceInfo);

    /// Changes this [`docker_compose_types::Service`] `depends_on` section to
    /// `depends_on: mirrord-sidecar`.
    fn modify_depends_on(&mut self);
}

impl ServiceExt for docker_compose_types::Service {
    fn modify_environment(&mut self, service_info: &ServiceInfo) {
        use docker_compose_types::Environment;

        if self.environment.is_empty() {
            self.environment = Environment::KvPair(Default::default());
        }

        match &mut self.environment {
            Environment::KvPair(index_map) => {
                index_map.extend(
                    service_info
                        .env_vars
                        .iter()
                        // TODO(alex) [mid] [#4]: Remove this.
                        .filter(|(k, _)| !k.contains("LOCALSTACK"))
                        .map(|(k, v)| (k.to_owned(), serde_yaml::from_str(&v.to_string()).ok())),
                );
            }
            _ => unreachable!(
                "BUG! It should only be `Environment::List` when \
                `environment` is empty, but we just set it to `Environment::KvPair`!"
            ),
        }
    }

    fn modify_depends_on(&mut self) {
        use docker_compose_types::{DependsCondition, DependsOnOptions};

        if self.depends_on.is_empty() {
            self.depends_on = DependsOnOptions::Conditional(Default::default());
        }

        match &mut self.depends_on {
            DependsOnOptions::Conditional(index_map) => {
                index_map.insert(
                    MIRRORD_COMPOSE_SIDECAR_SERVICE.into(),
                    DependsCondition {
                        condition: "service_started".to_owned(),
                    },
                );
            }
            _ => unreachable!(
                "BUG! It should only be `DependsOnOptions::Simple` when \
                `depends_on` is empty, but we just set it to `DependsOnOptions::Conditional`!"
            ),
        }
    }
}
