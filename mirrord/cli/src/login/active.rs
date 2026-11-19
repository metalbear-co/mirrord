//! The operator login a run uses, applied to the config it resolved.

use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::{
    client::{check_config_under_login, check_operator_enabled_under_login},
    sso::OperatorLogin,
};

use crate::{
    data::{AuthStore, GlobalConfig},
    error::CliResult,
};

/// Reads the operator login mirrord uses, merges the global defaults into `config`, and makes the
/// result fit the login: `operator: false` is refused, `kubeconfig` and `kube_context` get a
/// warning in `cfg_context` naming them, and the operator is required, so no part of the run
/// falls back to the kubeconfig when the operator is not found.
///
/// The warnings look at the config before the global defaults go in: a global `kube_context` is
/// a default the login makes meaningless, not a choice to flag. The refusal looks at the merged
/// config, since a global `operator: false` is only there.
///
/// `None` when mirrord uses the kubeconfig, in which case only the global defaults are applied.
/// Runs once per command, where the config is resolved: the operator client reads the same login
/// again when it builds its client, so nothing else needs the login passed along.
pub(crate) async fn apply_login_and_global_defaults(
    config: &mut LayerConfig,
    cfg_context: &mut ConfigContext,
    global: &GlobalConfig,
) -> CliResult<Option<OperatorLogin>> {
    let login = AuthStore::active_operator_login().await?;

    if let Some(login) = &login {
        for warning in check_config_under_login(config, login)? {
            // Commands that print no config warnings still leave a trace of the ignored setting.
            tracing::warn!("{warning}");
            cfg_context.add_warning(warning);
        }
    }

    global.apply_to(config);

    let Some(login) = login else {
        return Ok(None);
    };
    check_operator_enabled_under_login(config, &login)?;
    config.operator = Some(true);

    Ok(Some(login))
}
