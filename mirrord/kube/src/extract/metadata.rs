use std::sync::Arc;

use kube::Resource;

use super::{FromResource, OptionalFromResource};
use crate::error::KubeApiError;

/// Resource's `metadata.name` value, cannot be empty.
#[derive(Debug)]
pub struct Name(pub String);

impl<R, C> FromResource<R, C> for Name
where
    R: Resource<DynamicType = ()> + Send + Sync,
    C: Send + Sync,
{
    type Rejection = KubeApiError;

    fn from_resource(
        resource: &R,
        _: Arc<C>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            resource
                .meta()
                .name
                .clone()
                .map(Name)
                .ok_or_else(|| KubeApiError::missing_field(resource, ".metadata.name")),
        )
    }
}

impl<R, C> OptionalFromResource<R, C> for Name
where
    R: Resource + Send + Sync,
    C: Send + Sync,
{
    type Rejection = KubeApiError;

    fn from_resource(
        resource: &R,
        _: Arc<C>,
    ) -> impl Future<Output = Result<Option<Self>, Self::Rejection>> + Send {
        std::future::ready(Ok(resource.meta().name.clone().map(Name)))
    }
}

/// Resource's `metadata.namespace` value, cannot be empty.
pub struct Namespace(pub String);

impl<R, C> FromResource<R, C> for Namespace
where
    R: Resource<DynamicType = ()> + Send + Sync,
    C: Send + Sync,
{
    type Rejection = KubeApiError;

    fn from_resource(
        resource: &R,
        _: Arc<C>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            resource
                .meta()
                .namespace
                .clone()
                .map(Namespace)
                .ok_or_else(|| KubeApiError::missing_field(resource, ".metadata.namespace")),
        )
    }
}

impl<R, C> OptionalFromResource<R, C> for Namespace
where
    R: Resource + Send + Sync,
    C: Send + Sync,
{
    type Rejection = KubeApiError;

    fn from_resource(
        resource: &R,
        _: Arc<C>,
    ) -> impl Future<Output = Result<Option<Self>, Self::Rejection>> + Send {
        std::future::ready(Ok(resource.meta().namespace.clone().map(Namespace)))
    }
}

/// Resource's `metadata.uid` value, cannot be empty.
pub struct Uid(pub String);

impl<R, C> FromResource<R, C> for Uid
where
    R: Resource<DynamicType = ()> + Send + Sync,
    C: Send + Sync,
{
    type Rejection = KubeApiError;

    fn from_resource(
        resource: &R,
        _: Arc<C>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            resource
                .meta()
                .uid
                .clone()
                .map(Uid)
                .ok_or_else(|| KubeApiError::missing_field(resource, ".metadata.namespace")),
        )
    }
}

impl<R, C> OptionalFromResource<R, C> for Uid
where
    R: Resource + Send + Sync,
    C: Send + Sync,
{
    type Rejection = KubeApiError;

    fn from_resource(
        resource: &R,
        _: Arc<C>,
    ) -> impl Future<Output = Result<Option<Self>, Self::Rejection>> + Send {
        std::future::ready(Ok(resource.meta().namespace.clone().map(Uid)))
    }
}

#[cfg(test)]
mod tests {

    use std::sync::LazyLock;

    use k8s_openapi::api::core::v1::Pod;
    use kube::api::ObjectMeta;

    use super::*;

    static TEST_RESOURCE: LazyLock<Arc<Pod>> = LazyLock::new(|| {
        Arc::new(Pod {
            metadata: ObjectMeta {
                name: Some("foo".into()),
                namespace: Some("default".into()),
                ..Default::default()
            },
            ..Default::default()
        })
    });

    #[tokio::test]
    async fn extract_name() {
        let Name(name) =
            <Name as FromResource<Pod, ()>>::from_resource(&TEST_RESOURCE, Arc::<()>::default())
                .await
                .expect("should be extracted");

        assert_eq!(name, "foo");
    }

    #[tokio::test]
    async fn extract_namespace() {
        let Namespace(namespace) = <Namespace as FromResource<Pod, ()>>::from_resource(
            &TEST_RESOURCE,
            Arc::<()>::default(),
        )
        .await
        .expect("should be extracted");

        assert_eq!(namespace, "default");
    }
}
