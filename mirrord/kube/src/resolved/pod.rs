use k8s_openapi::api::core::v1::Pod;

use super::ResolvedResource;
use crate::{api::runtime::RuntimeData, error::Result};

impl ResolvedResource<Pod> {
    pub async fn runtime_data(&self) -> Result<crate::api::runtime::RuntimeData> {
        RuntimeData::from_pod(&self.resource, self.container.as_deref())
    }
}
