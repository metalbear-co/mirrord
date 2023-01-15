use std::{fs::File, path::PathBuf};

use mirrord_operator::{
    license::License,
    setup::{Operator, OperatorNamespace, OperatorSetup},
};

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

/// Handle commands related to the operator `mirrord operator ...`
pub(crate) async fn operator_command(args: OperatorArgs) -> Result<()> {
    match args.command {
        OperatorCommand::Setup {
            accept_tos,
            file,
            namespace,
            license_key,
        } => operator_setup(accept_tos, file, namespace, license_key).await,
    }
}
