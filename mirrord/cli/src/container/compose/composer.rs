use super::{steps::New, ServiceInfo, MIRRORD_COMPOSE_SIDECAR_SERVICE};

pub(super) struct ServiceComposer<'a, Step> {
    step: Step,
    service: &'a mut docker_compose_types::Service,
}

pub(super) struct ResetConflicts {
    ports: docker_compose_types::Ports,
}

impl<'a> ServiceComposer<'a, New> {
    pub(super) fn modify(
        service: &'a mut docker_compose_types::Service,
        service_info: &ServiceInfo,
    ) -> ServiceComposer<'a, ResetConflicts> {
        service.modify_environment(service_info);
        service.modify_depends_on();

        service
            .volumes_from
            .push(MIRRORD_COMPOSE_SIDECAR_SERVICE.into());
        service.network_mode = Some(format!("service:{MIRRORD_COMPOSE_SIDECAR_SERVICE}"));

        ServiceComposer {
            step: ResetConflicts {
                ports: service.ports.clone(),
            },
            service,
        }
    }
}

impl<'a> ServiceComposer<'a, ResetConflicts> {
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

trait ServiceExt {
    fn modify_environment(&mut self, service_info: &ServiceInfo);
    fn modify_depends_on(&mut self);
}

impl ServiceExt for docker_compose_types::Service {
    fn modify_environment(&mut self, service_info: &ServiceInfo) {
        match &mut self.environment {
            docker_compose_types::Environment::KvPair(index_map) => {
                index_map.extend(
                    service_info
                        .env_vars
                        .iter()
                        // TODO(alex) [mid] [#4]: Remove this.
                        .filter(|(k, _)| !k.contains("LOCALSTACK"))
                        .map(|(k, v)| (k.to_owned(), serde_yaml::from_str(&format!("{v}")).ok())),
                );
            }
            // When a service has no `environment`, it gets built by default as
            // `Environment::List`, so we just ignore it (ignore on empty).
            _ => (),
        }
    }

    fn modify_depends_on(&mut self) {
        use docker_compose_types::DependsCondition;

        match &mut self.depends_on {
            docker_compose_types::DependsOnOptions::Conditional(index_map) => {
                index_map.insert(
                    MIRRORD_COMPOSE_SIDECAR_SERVICE.into(),
                    DependsCondition {
                        condition: "service_started".to_owned(),
                    },
                );
            }
            // When a service has no `depends_on`, it gets built by default as
            // `DependsOnOption::Simple`, so we just ignore it (ignore on empty).
            _ => (),
        }
    }
}
