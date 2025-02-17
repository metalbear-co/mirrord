use mirrord_analytics::AnalyticsReporter;
use mirrord_config::LayerConfig;
use tempfile::NamedTempFile;

use crate::container::command_builder::RuntimeCommandBuilder;

#[derive(Debug)]
pub(crate) struct New;

#[derive(Debug)]
pub(crate) struct PrepareConfigAndAnalytics;

#[derive(Debug)]
pub(crate) struct PrepareTLS {
    pub(super) config: LayerConfig,
    pub(super) analytics: AnalyticsReporter,
}

#[derive(Debug)]
pub(crate) struct PrepareLayerConfig {
    pub(super) internal_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) external_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
}

#[derive(Debug)]
pub(crate) struct PrepareExternalProxy {
    pub(super) internal_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) external_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
    pub(super) layer_config_file: NamedTempFile,
}

#[derive(Debug)]
pub(crate) struct PrepareCompose {
    pub(super) internal_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) external_proxy_tls_guards: Option<(NamedTempFile, NamedTempFile)>,
    pub(super) analytics: AnalyticsReporter,
    pub(super) config: LayerConfig,
    pub(super) layer_config_file: NamedTempFile,
    pub(super) runtime_command_builder: RuntimeCommandBuilder,
}
