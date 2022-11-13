use std::{collections::HashSet, sync::LazyLock};

use k8s_openapi::api::core::v1::ContainerStatus;

use crate::error::Result;

pub trait ContainerApi {
    fn create_agent() -> Result<String>;
}

pub const SKIP_NAMES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["istio-proxy", "linkerd-proxy", "proxy-init", "istio-init"]));

/// Choose container logic:
/// 1. Try to find based on given name
/// 2. Try to find first container in pod that isn't a mesh side car
/// 3. Take first container in pod
pub fn choose_container<'a>(
    container_name: &Option<String>,
    container_statuses: &'a [ContainerStatus],
) -> Option<&'a ContainerStatus> {
    if let Some(name) = container_name {
        container_statuses
            .iter()
            .find(|&status| &status.name == name)
    } else {
        // Choose any container that isn't part of the skip list
        container_statuses
            .iter()
            .find(|&status| !SKIP_NAMES.contains(status.name.as_str()))
            .or_else(|| container_statuses.first())
    }
}

#[derive(Debug)]
pub struct JobContainer;

impl ContainerApi for JobContainer {
    fn create_agent() -> Result<String> {
        todo!()
    }
}

#[derive(Debug)]
pub struct EphemeralContainer;

impl ContainerApi for EphemeralContainer {
    fn create_agent() -> Result<String> {
        todo!()
    }
}
