use std::sync::Arc;

pub mod metadata;

/// Types that can be created from kubernetes resource
pub trait FromResource<'r, R, C>: Sized {
    /// "rejection" type that will be used if extraction is failed.
    type Rejection;

    fn from_resource(resource: &'r R, context: Arc<C>) -> Result<Self, Self::Rejection>;
}

/// Types that can be created from kubernetes resource but are optional.
pub trait OptionalFromResource<'r, R, C>: Sized {
    /// "rejection" type that will be used if extraction is failed.
    type Rejection;

    fn from_resource(resource: &'r R, context: Arc<C>) -> Result<Option<Self>, Self::Rejection>;
}

impl<'r, T, R, C> FromResource<'r, R, C> for Option<T>
where
    T: OptionalFromResource<'r, R, C>,
{
    type Rejection = T::Rejection;

    fn from_resource(resource: &'r R, context: Arc<C>) -> Result<Self, Self::Rejection> {
        T::from_resource(resource, context)
    }
}

macro_rules! impl_tuple_from_resource {
	(
        [$($ty:ident),*], $last:ident
    ) => {
    	#[allow(non_snake_case)]
    	impl<'r, R, C, Rej, $($ty,)* $last> FromResource<'r, R, C> for ($($ty,)* $last,)
		where
			$( $ty: FromResource<'r, R, C, Rejection = Rej>, )*
            $last: FromResource<'r, R, C, Rejection = Rej>,
		{
		    type Rejection = Rej;

		    #[allow(non_snake_case)]
		    fn from_resource(resource: &'r R, context: Arc<C>) -> Result<Self, Self::Rejection> {
		    	$(
                    let $ty = $ty::from_resource(resource.clone(), context.clone())?;
                )*
                let $last = $last::from_resource(resource, context)?;

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

    impl<R, C> FromResource<'_, R, C> for TestField {
        type Rejection = Infallible;

        fn from_resource(_: &R, _cx: Arc<C>) -> Result<Self, Self::Rejection> {
            Ok(TestField)
        }
    }

    struct TestField2;

    impl<R, C> FromResource<'_, R, C> for TestField2 {
        type Rejection = Infallible;

        fn from_resource(_: &R, _cx: Arc<C>) -> Result<Self, Self::Rejection> {
            Ok(TestField2)
        }
    }

    #[test]
    fn single_field() {
        let resource = Pod::default();

        assert!(TestField::from_resource(&resource, Arc::new(())).is_ok());
    }

    #[test]
    fn multiple_fields() {
        let resource = Pod::default();

        assert!(<(TestField, TestField2)>::from_resource(&resource, Arc::new(())).is_ok())
    }
}
