use std::{fs::File, path::PathBuf, time::Duration};

use kube::Api;
use mirrord_config::{config::MirrordConfig, LayerFileConfig};
use mirrord_kube::{api::kubernetes::create_kube_api, error::KubeApiError};
use mirrord_operator::{
    client::OperatorApiError,
    crd::{MirrordOperatorCrd, MirrordOperatorSpec, OPERATOR_STATUS_NAME},
    license::License,
    setup::{Operator, OperatorNamespace, OperatorSetup},
};
use mirrord_progress::{Progress, TaskProgress};
use prettytable::{row, Table};

use crate::{
    config::{OperatorArgs, OperatorCommand},
    error::CliError,
    Result,
};

/// Setup the operator into a file or to stdout, with explanation.
async fn operator_setup(
    accept_tos: bool,
    file: Option<PathBuf>,
    namespace: OperatorNamespace,
    license_key: Option<String>,
) -> Result<()> {
    if !accept_tos {
        eprintln!("Please note that mirrord operator installation requires an active subscription for the mirrord Operator provided by MetalBear Tech LTD.\nThe service ToS can be read here - https://metalbear.co/legal/terms\nPass --accept-tos to accept the TOS");

        return Ok(());
    }

    if let Some(license_key) = license_key {
        let license = License::fetch_async(license_key.clone())
            .await
            .map_err(CliError::LicenseError)?;

        eprintln!(
            "Installing with license for {} ({})",
            license.name, license.organization
        );

        if license.is_expired() {
            eprintln!("Using an expired license for operator, deployment will not function when installed");
        }

        eprintln!(
            "Intalling mirrord operator with namespace: {}",
            namespace.name()
        );

        let operator = Operator::new(license_key, namespace);

        match file {
            Some(path) => {
                operator.to_writer(File::create(path).map_err(CliError::ManifestFileError)?)?
            }
            None => operator.to_writer(std::io::stdout()).unwrap(), /* unwrap because failing to write to std out.. well.. */
        }
    } else {
        eprintln!("--license-key is required to install on cluster");
    }

    Ok(())
}

async fn operator_status(config: Option<String>) -> Result<()> {
    let progress = TaskProgress::new("Operator Status").fail_on_drop(true);

    let kube_api = if let Some(config_path) = config {
        let config = LayerFileConfig::from_path(config_path)?.generate_config()?;
        create_kube_api(config.accept_invalid_certificates, config.kubeconfig)
    } else {
        create_kube_api(false, None)
    }
    .await
    .map_err(CliError::KubernetesApiFailed)?;

    let status_api: Api<MirrordOperatorCrd> = Api::all(kube_api);

    let status_progress = progress.subtask("fetching status");

    let mirrord_status = match status_api
        .get(OPERATOR_STATUS_NAME)
        .await
        .map_err(KubeApiError::KubeError)
        .map_err(OperatorApiError::KubeApiError)
        .map_err(CliError::OperatorConnectionFailed)
    {
        Ok(status) => status,
        Err(err) => {
            status_progress.fail_with("unable to get status");

            return Err(err);
        }
    };

    status_progress.done_with("fetched status");

    progress.done();

    let MirrordOperatorSpec {
        operator_version,
        default_namespace,
        license:
            License {
                name,
                organization,
                expire_at,
            },
    } = mirrord_status.spec;

    let expire_at = expire_at.format("%e-%b-%Y");

    println!(
        r#"
Operator version: {operator_version}
Operator default namespace: {default_namespace}
Operator License
    name: {name}
    organization: {organization}
    expire at: {expire_at}
"#
    );

    let mut sessions = Table::new();

    sessions.add_row(row!["Session ID", "Target", "User", "Session Duration"]);

    if let Some(status) = mirrord_status.status {
        for session in &status.sessions {
            sessions.add_row(row![
                session.id.as_deref().unwrap_or(""),
                &session.target,
                &session.user,
                humantime::format_duration(Duration::from_secs(session.duration_secs)),
            ]);
        }
    }

    sessions.printstd();

    Ok(())
}

/// Handle commands related to the operator `mirrord operator ...`
pub(crate) async fn operator_command(args: OperatorArgs) -> Result<()> {
    match args.command {
        OperatorCommand::Setup {
            accept_tos,
            file,
            namespace,
            license_key,
        } => operator_setup(accept_tos, file, namespace, license_key).await,
        OperatorCommand::Status { config_file } => operator_status(config_file).await,
    }
}
