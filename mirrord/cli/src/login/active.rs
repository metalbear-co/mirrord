//! The operator login a run uses, applied to the config it resolved.

use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::{client::check_config_under_login, sso::OperatorLogin};

use crate::{data::AuthStore, error::CliResult};

/// Reads the operator login mirrord uses and makes `config` fit it: `operator: false` is refused,
/// `kubeconfig` and `kube_context` get a warning in `cfg_context` naming them, and the operator is
/// required, so no part of the run falls back to the kubeconfig when the operator is not found.
///
/// `None` when mirrord uses the kubeconfig, in which case the config is left as it is. Runs once
/// per command, where the config is resolved: the operator client reads the same login again when
/// it builds its client, so nothing else needs the login passed along.
pub(crate) async fn apply_active_operator_login(
    config: &mut LayerConfig,
    cfg_context: &mut ConfigContext,
) -> CliResult<Option<OperatorLogin>> {
    let Some(login) = AuthStore::active_operator_login().await? else {
        return Ok(None);
    };

    for warning in check_config_under_login(config, &login)? {
        cfg_context.add_warning(warning);
    }
    config.operator = Some(true);

    Ok(Some(login))
}
