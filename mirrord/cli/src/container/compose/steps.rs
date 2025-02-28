use std::net::SocketAddr;

use mirrord_analytics::AnalyticsReporter;
use mirrord_config::LayerConfig;
use tempfile::NamedTempFile;

use super::{service::ServiceInfo, TlsFileGuard};
use crate::MirrordExecution;

#[derive(Debug)]
pub(crate) struct New;

#[derive(Debug)]
pub(crate) struct PrepareConfig;

#[derive(Debug)]
pub(crate) struct PrepareExternalProxy {
    pub(super) internal_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) external_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
    pub(super) layer_config_file: NamedTempFile,
}

#[derive(Debug)]
pub(crate) struct PrepareServices {
    pub(super) internal_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) external_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
    pub(super) intproxy_address: SocketAddr,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) external_proxy: MirrordExecution,
}

#[derive(Debug)]
pub(crate) struct PrepareCompose {
    pub(super) internal_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) external_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) intproxy_port: u16,
    pub(super) sidecar_info: ServiceInfo,
    pub(super) user_service_info: ServiceInfo,
}

#[derive(Debug)]
pub(crate) struct RunCompose {
    pub(super) internal_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) external_proxy_tls_guards: Option<TlsFileGuard>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) compose_yaml: NamedTempFile,
}
