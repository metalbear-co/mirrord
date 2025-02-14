use http::Uri;
use rustls::pki_types::ServerName;

/// Utility trait for extracting [`ServerName`]s from [`Uri`]s.
pub trait UriExt {
    fn get_server_name(&self) -> Option<ServerName<'_>>;
}

impl UriExt for Uri {
    /// Attempts to extract a [`ServerName`] from this [`Uri`].
    ///
    /// Copied from [hyper-tls](https://github.com/hyperium/hyper-tls/blob/0265e166a8886f01253050516316a95900315b81/src/client.rs#L140).
    fn get_server_name(&self) -> Option<ServerName<'_>> {
        let hostname = self.host()?.trim_matches(|c| c == '[' || c == ']');

        ServerName::try_from(hostname).ok()
    }
}
