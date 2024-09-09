use mirrord_analytics::Reporter;
use mirrord_config::LayerConfig;
use mirrord_operator::client::OperatorApi;
use mirrord_progress::Progress;

use crate::CliError;

pub(super) struct OperatorConnection<'a, P, R>
where
    P: Progress,
    R: Reporter,
{
    config: &'a LayerConfig,
    progress: &'a P,
    analytics: &'a mut R,
}

impl<'a, P, R> OperatorConnection<'a, P, R>
where
    P: Progress,
    R: Reporter,
{
    pub(super) fn new(
        config: &'a LayerConfig,
        progress: &'a P,
        analytics: &'a mut R,
    ) -> Option<Self> {
        let mut operator_subtask = progress.subtask("checking operator");
        if config.operator == Some(false) {
            operator_subtask.success(Some("operator disabled"));
            None
        } else {
            Some(Self {
                config,
                progress,
                analytics,
            })
        }
    }

    pub(super) fn create_api(&mut self) {}
}
