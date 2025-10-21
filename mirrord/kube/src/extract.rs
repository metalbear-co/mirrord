use std::sync::Arc;

pub mod metadata;

/// Types that can be created from kubernetes resource
pub trait FromResource<R, C>: Sized {
    /// "rejection" type that will be used if extraction is failed.
    type Rejection;

    fn from_resource(
        resource: &R,
        context: Arc<C>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send;
}

/// Types that can be created from kubernetes resource but are optional.
pub trait OptionalFromResource<R, C>: Sized {
    /// "rejection" type that will be used if extraction is failed.
    type Rejection;

    fn from_resource(
        resource: &R,
        context: Arc<C>,
    ) -> impl Future<Output = Result<Option<Self>, Self::Rejection>> + Send;
}

impl<T, R, C> FromResource<R, C> for Option<T>
where
    T: OptionalFromResource<R, C>,
{
    type Rejection = T::Rejection;

    fn from_resource(
        resource: &R,
        context: Arc<C>,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        T::from_resource(resource, context)
    }
}

macro_rules! impl_tuple_from_resource {
	(
        [$($ty:ident),*], $last:ident
    ) => {
    	#[allow(non_snake_case)]
    	impl<R, C, Rej, $($ty,)* $last> FromResource<R, C> for ($($ty,)* $last,)
		where
			$( $ty: FromResource<R, C, Rejection = Rej> + Send, )*
            $last: FromResource<R, C, Rejection = Rej> + Send,
		    R: Send + Sync,
		    C: Send + Sync,
		{
		    type Rejection = Rej;

		    #[allow(non_snake_case)]
		    async fn from_resource(resource: &R, context: Arc<C>) -> Result<Self, Self::Rejection> {
		    	$(
                    let $ty = $ty::from_resource(resource.clone(), context.clone()).await?;
                )*
                let $last = $last::from_resource(resource, context).await?;

		        Ok(($($ty,)* $last,))
		    }
		}
    }
}

impl_tuple_from_resource!([], T1);
impl_tuple_from_resource!([T1], T2);
impl_tuple_from_resource!([T1, T2], T3);
impl_tuple_from_resource!([T1, T2, T3], T4);
impl_tuple_from_resource!([T1, T2, T3, T4], T5);
impl_tuple_from_resource!([T1, T2, T3, T4, T5], T6);
impl_tuple_from_resource!([T1, T2, T3, T4, T5, T6], T7);
impl_tuple_from_resource!([T1, T2, T3, T4, T5, T6, T7], T8);
impl_tuple_from_resource!([T1, T2, T3, T4, T5, T6, T7, T8], T9);
impl_tuple_from_resource!([T1, T2, T3, T4, T5, T6, T7, T8, T9], T10);
impl_tuple_from_resource!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10], T11);
impl_tuple_from_resource!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11], T12);
impl_tuple_from_resource!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12], T13);
impl_tuple_from_resource!(
    [T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13],
    T14
);
impl_tuple_from_resource!(
    [T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14],
    T15
);
impl_tuple_from_resource!(
    [
        T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15
    ],
    T16
);

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use k8s_openapi::api::core::v1::Pod;

    use super::*;

    struct TestField;

    impl<R, C> FromResource<R, C> for TestField
    where
        R: Send + Sync,
        C: Send + Sync,
    {
        type Rejection = Infallible;

        async fn from_resource(_: &R, _cx: Arc<C>) -> Result<Self, Self::Rejection> {
            Ok(TestField)
        }
    }

    struct TestField2;

    impl<R, C> FromResource<R, C> for TestField2
    where
        R: Send + Sync,
        C: Send + Sync,
    {
        type Rejection = Infallible;

        async fn from_resource(_: &R, _cx: Arc<C>) -> Result<Self, Self::Rejection> {
            Ok(TestField2)
        }
    }

    #[tokio::test]
    async fn single_field() {
        let resource = Pod::default();

        assert!(
            TestField::from_resource(&resource, Arc::new(()))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn multiple_fields() {
        let resource = Pod::default();

        assert!(
            <(TestField, TestField2)>::from_resource(&resource, Arc::new(()))
                .await
                .is_ok()
        )
    }
}
