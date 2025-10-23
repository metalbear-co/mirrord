use kube::Resource;

use super::{FromResource, OptionalFromResource};
use crate::error::KubeApiError;

/// Resource's `metadata.name` value, cannot be empty.
#[derive(Debug)]
pub struct Name<'r>(pub &'r str);

impl<'r> Name<'r> {
    pub fn into_inner(self) -> &'r str {
        self.0
    }
}

impl<'r, R, C> FromResource<'r, R, C> for Name<'r>
where
    R: Resource<DynamicType = ()>,
{
    type Rejection = KubeApiError;

    fn from_resource(resource: &'r R, _: &C) -> Result<Self, Self::Rejection> {
        resource
            .meta()
            .name
            .as_deref()
            .map(Name)
            .ok_or_else(|| KubeApiError::missing_field(resource, ".metadata.name"))
    }
}

/// Resource's `metadata.namespace` value, cannot be empty.
pub struct Namespace<'r>(pub &'r str);

impl<'r> Namespace<'r> {
    pub fn into_inner(self) -> &'r str {
        self.0
    }
}

impl<'r, R, C> FromResource<'r, R, C> for Namespace<'r>
where
    R: Resource<DynamicType = ()>,
{
    type Rejection = KubeApiError;

    fn from_resource(resource: &'r R, _: &C) -> Result<Self, Self::Rejection> {
        resource
            .meta()
            .namespace
            .as_deref()
            .map(Namespace)
            .ok_or_else(|| KubeApiError::missing_field(resource, ".metadata.namespace"))
    }
}

impl<'r, R, C> OptionalFromResource<'r, R, C> for Namespace<'r>
where
    R: Resource,
{
    type Rejection = KubeApiError;

    fn from_resource(resource: &'r R, _: &C) -> Result<Option<Self>, Self::Rejection> {
        Ok(resource.meta().namespace.as_deref().map(Namespace))
    }
}

/// Resource's `metadata.uid` value, cannot be empty.
pub struct Uid<'r>(pub &'r str);

impl<'r> Uid<'r> {
    pub fn into_inner(self) -> &'r str {
        self.0
    }
}

impl<'r, R, C> FromResource<'r, R, C> for Uid<'r>
where
    R: Resource<DynamicType = ()>,
{
    type Rejection = KubeApiError;

    fn from_resource(resource: &'r R, _: &C) -> Result<Self, Self::Rejection> {
        resource
            .meta()
            .uid
            .as_deref()
            .map(Uid)
            .ok_or_else(|| KubeApiError::missing_field(resource, ".metadata.namespace"))
    }
}

impl<'r, R, C> OptionalFromResource<'r, R, C> for Uid<'r>
where
    R: Resource,
{
    type Rejection = KubeApiError;

    fn from_resource(resource: &'r R, _: &C) -> Result<Option<Self>, Self::Rejection> {
        Ok(resource.meta().namespace.as_deref().map(Uid))
    }
}

#[cfg(test)]
mod tests {

    use std::sync::LazyLock;

    use k8s_openapi::api::core::v1::Pod;
    use kube::api::ObjectMeta;

    use super::*;

    static TEST_RESOURCE: LazyLock<Pod> = LazyLock::new(|| Pod {
        metadata: ObjectMeta {
            name: Some("foo".into()),
            namespace: Some("default".into()),
            ..Default::default()
        },
        ..Default::default()
    });

    #[test]
    fn extract_name() {
        let Name(name) =
            FromResource::from_resource(&*TEST_RESOURCE, &()).expect("should be extracted");

        assert_eq!(name, "foo");
    }

    #[test]
    fn extract_namespace() {
        let Namespace(namespace) =
            FromResource::from_resource(&*TEST_RESOURCE, &()).expect("should be extracted");

        assert_eq!(namespace, "default");
    }
}
