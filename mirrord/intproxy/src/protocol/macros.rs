/// A helper macro for binding inner-most values from complex enum expressions.
/// Accepts an identifier and a non-empty sequence of enum paths.
///
/// Used in [`impl_request`] macro for `match` expressions.
///
/// # Example
///
/// ```
/// use mirrord_intproxy::bind_nested;
///
/// enum A {
///     B(B),
/// }
///
/// enum B {
///     C(usize),
/// }
///
/// let val = A::B(B::C(1));
/// let bind_nested!(inner, A::B, B::C) = val;
///
/// assert_eq!(inner, 1);
/// ```
#[macro_export]
macro_rules! bind_nested {
    ($bind_to: ident, $variant: path, $($rest: path),+) => {
        $variant(bind_nested!($bind_to, $($rest),+))
    };

    ($bind_to: ident, $variant: path) => { $variant($bind_to) };
}

/// A helper macro for implementing [`IsLayerRequest`](super::IsLayerRequest) and
/// [`IsLayerRequestWithResponse`](super::IsLayerRequestWithResponse) traits. Accepts arguments in
/// two forms, see invocations in [`crate::protocol`] for
/// [`OpenFileRequest`](mirrord_protocol::file::OpenFileRequest) and
/// [`CloseFileRequest`](mirrord_protocol::file::CloseFileRequest) below in this file. Invocation
/// for [`OpenFileRequest`](mirrord_protocol::file::OpenFileRequest) generates both
/// [`IsLayerRequest`](super::IsLayerRequest) and
/// [`IsLayerRequestWithResponse`](super::IsLayerRequestWithResponse) traits. Invocation for
/// [`CloseFileRequest`] generates only [`IsLayerRequest`](super::IsLayerRequest) trait.
#[macro_export]
macro_rules! impl_request {
    (
        req = $req_type: path,
        res = $res_type: path,
        req_path = $($req_variants: path) => +,
        res_path = $($res_variants: path) => +,
    ) => {
        impl_request!(
            req = $req_type,
            req_path = $($req_variants) => +,
        );

        impl IsLayerRequestWithResponse for $req_type {
            type Response = $res_type;

            fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
                bind_nested!(response, $($res_variants),+)
            }

            fn check_response(response: &ProxyToLayerMessage) -> bool {
                match response {
                    bind_nested!(_inner, $($res_variants),+) => true,
                    _ => false,
                }
            }

            fn try_unwrap_response(response: ProxyToLayerMessage) -> Result<Self::Response, ProxyToLayerMessage> {
                match response {
                    bind_nested!(inner, $($res_variants),+) => Ok(inner),
                    other => Err(other),
                }
            }
        }
    };

    (
        req = $req_type: path,
        req_path = $($req_variants: path) => +,
    ) => {
        impl IsLayerRequest for $req_type {
            fn wrap(self) -> LayerToProxyMessage {
                bind_nested!(self, $($req_variants),+)
            }

            fn check(message: &LayerToProxyMessage) -> bool {
                match message {
                    bind_nested!(_inner, $($req_variants),+) => true,
                    _ => false,
                }
            }

            fn try_unwrap(message: LayerToProxyMessage) -> Result<Self, LayerToProxyMessage> {
                match message {
                    bind_nested!(inner, $($req_variants),+) => Ok(inner),
                    other => Err(other),
                }
            }
        }
    };
}
