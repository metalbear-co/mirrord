//! `mirrord verify`: runs a test command and keeps a record of the run on the cluster.

use std::{ops::Not, path::PathBuf, time::Duration};

use chrono::{SecondsFormat, Utc};
use kube::{
    Api,
    api::{ListParams, ObjectMeta, Patch, PatchParams, PostParams},
};
use mirrord_analytics::NullReporter;
use mirrord_command::resolve_tokio_command;
use mirrord_config::{LayerConfig, config::ConfigContext, env_key::MIRRORD_ENV_KEY};
use mirrord_operator::{
    client::OperatorApi,
    crd::{
        NewOperatorFeature,
        verification::{
            MirrordVerification, MirrordVerificationSpec, VERIFICATION_COMMIT_LABEL,
            VERIFICATION_KEY_PREFIX, VerificationClientReport, VerificationPhase, verification_key,
        },
    },
    types::{OPERATOR_OWNERSHIP_LABEL, isolation_marker},
};
use mirrord_progress::NullProgress;
use rand::distr::{Alphanumeric, SampleString};
use serde_json::json;
use tokio::time::{Instant, sleep};

use crate::{
    config::verify::{VerifyArgs, VerifyCommand, VerifyLookupArgs, VerifyRunArgs},
    error::{CliError, CliResult},
};

pub(crate) mod error;
mod git;

use error::VerifyError;

/// Length of a full commit hash.
const COMMIT_LEN: usize = 40;

/// How long `run` waits for the operator's verdict after the command has exited.
const VERDICT_TIMEOUT: Duration = Duration::from_secs(30);

/// Exit code of `check` when no verification of the commit passed.
const EXIT_NOT_VERIFIED: i32 = 1;

/// Exit code of `check` when the records can't be read.
const EXIT_NO_RECORDS: i32 = 2;

pub(crate) async fn verify_command(args: VerifyArgs) -> CliResult<()> {
    match args.command {
        VerifyCommand::Run(args) => run(args).await,
        VerifyCommand::Show(args) => show(args).await,
        VerifyCommand::Check(args) => check(args).await,
    }
}

/// Runs the command with a verification key as its session key, and exits with its exit code.
async fn run(args: VerifyRunArgs) -> CliResult<()> {
    let facts = git::git_facts().await?;
    let suffix = Alphanumeric
        .sample_string(&mut rand::rng(), 4)
        .to_lowercase();
    let key = verification_key(&facts.commit, &suffix);
    let command = args.command.join(" ");

    let spec = MirrordVerificationSpec {
        repository: facts.repository,
        commit: facts.commit.clone(),
        tree: facts.tree,
        dirty: facts.dirty,
        branch: facts.branch,
        command: command.clone(),
        client_cli_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_report: None,
    };

    let api = match create_record(args.config_file, &key, spec).await {
        Ok(api) => Some(api),
        Err(error) => {
            eprintln!("mirrord verify: running without a verification record: {error}");
            None
        }
    };

    let (program, program_args) = args
        .command
        .split_first()
        .ok_or(VerifyError::EmptyCommand)?;
    let status = resolve_tokio_command(program)
        .args(program_args)
        .env(MIRRORD_ENV_KEY, &key)
        .status()
        .await
        .map_err(|error| VerifyError::CommandNotRun {
            command: command.clone(),
            error,
        })?;
    // A command killed by a signal has no exit code.
    let exit_code = status.code().unwrap_or(1);

    if let Some(api) = api {
        match report(&api, &key, exit_code).await {
            Ok(record) => print_record(&record),
            Err(error) => {
                eprintln!("mirrord verify: the verification record `{key}` is incomplete: {error}")
            }
        }
    }

    std::process::exit(exit_code)
}

async fn show(args: VerifyLookupArgs) -> CliResult<()> {
    let api = verification_api(args.config_file).await?;
    let records = find(&api, &args.reference).await?;
    if records.is_empty() {
        return Err(VerifyError::NoRecord(args.reference).into());
    }

    records.iter().for_each(print_record);
    Ok(())
}

async fn check(args: VerifyLookupArgs) -> CliResult<()> {
    if is_full_commit(&args.reference).not() {
        return Err(VerifyError::NotAFullCommit(args.reference).into());
    }

    let records = match verification_api(args.config_file).await {
        Ok(api) => find(&api, &args.reference).await,
        Err(error) => Err(error),
    };
    let records = match records {
        Ok(records) => records,
        Err(error) => {
            eprintln!("mirrord verify: can't read verification records: {error}");
            std::process::exit(EXIT_NO_RECORDS)
        }
    };

    if let Some(passed) = records
        .iter()
        .find(|record| phase(record) == VerificationPhase::Passed)
    {
        print_record(passed);
        return Ok(());
    }

    if records.is_empty() {
        eprintln!(
            "mirrord verify: no verification record for commit {}",
            args.reference
        );
    }
    records.iter().for_each(print_record);
    std::process::exit(EXIT_NOT_VERIFIED)
}

/// The API for verification records, when the operator of the configured cluster keeps them.
async fn verification_api(config_file: Option<PathBuf>) -> CliResult<Api<MirrordVerification>> {
    let mut cfg_context =
        ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, config_file);
    let layer_config = crate::util::resolve_layer_config(&mut cfg_context).await?;

    let operator_api =
        OperatorApi::try_new(&layer_config, &mut NullReporter::default(), &NullProgress)
            .await?
            .ok_or(CliError::OperatorNotInstalled)?;
    operator_api.check_license_validity(&NullProgress)?;
    operator_api
        .operator()
        .spec
        .require_feature(NewOperatorFeature::Verification)?;

    Ok(Api::all(operator_api.client().clone()))
}

async fn create_record(
    config_file: Option<PathBuf>,
    key: &str,
    spec: MirrordVerificationSpec,
) -> CliResult<Api<MirrordVerification>> {
    let api = verification_api(config_file).await?;

    let record = MirrordVerification {
        metadata: ObjectMeta {
            name: Some(key.to_owned()),
            labels: Some(
                [
                    (VERIFICATION_COMMIT_LABEL.to_owned(), spec.commit.clone()),
                    (
                        OPERATOR_OWNERSHIP_LABEL.to_owned(),
                        isolation_marker().to_owned(),
                    ),
                ]
                .into(),
            ),
            ..Default::default()
        },
        spec,
        status: None,
    };
    api.create(&PostParams::default(), &record)
        .await
        .map_err(VerifyError::Kube)?;

    Ok(api)
}

/// Adds the command's exit code to the record, then waits for the operator's verdict.
async fn report(
    api: &Api<MirrordVerification>,
    key: &str,
    exit_code: i32,
) -> Result<MirrordVerification, VerifyError> {
    let client_report = VerificationClientReport {
        exit_code,
        finished_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    };
    let patch = json!({ "spec": { "clientReport": client_report } });
    let mut record = api
        .patch(key, &PatchParams::default(), &Patch::Merge(patch))
        .await?;

    let deadline = Instant::now() + VERDICT_TIMEOUT;
    while phase(&record) == VerificationPhase::Running && Instant::now() < deadline {
        sleep(Duration::from_secs(1)).await;
        record = api.get(key).await?;
    }

    Ok(record)
}

/// The records `reference` names: one record by its key, or every record of a commit.
async fn find(
    api: &Api<MirrordVerification>,
    reference: &str,
) -> CliResult<Vec<MirrordVerification>> {
    if reference.starts_with(VERIFICATION_KEY_PREFIX) {
        let record = api.get_opt(reference).await.map_err(VerifyError::Kube)?;
        return Ok(record.into_iter().collect());
    }

    let params = if is_full_commit(reference) {
        ListParams::default().labels(&format!("{VERIFICATION_COMMIT_LABEL}={reference}"))
    } else {
        ListParams::default()
    };
    let records = api.list(&params).await.map_err(VerifyError::Kube)?;

    Ok(records
        .into_iter()
        .filter(|record| record.spec.commit.starts_with(reference))
        .collect())
}

fn is_full_commit(reference: &str) -> bool {
    reference.len() == COMMIT_LEN && reference.chars().all(|c| c.is_ascii_hexdigit())
}

fn phase(record: &MirrordVerification) -> VerificationPhase {
    record
        .status
        .as_ref()
        .map(|status| status.phase)
        .unwrap_or_default()
}

fn print_record(record: &MirrordVerification) {
    let spec = &record.spec;
    let sessions = record
        .status
        .as_ref()
        .map(|status| status.sessions.as_slice())
        .unwrap_or_default();
    let checkout = if spec.dirty {
        "uncommitted changes"
    } else {
        "clean"
    };

    println!("mirrord verify: {}", phase(record));
    println!(
        "  record    {}",
        record.metadata.name.as_deref().unwrap_or_default()
    );
    println!("  commit    {} ({checkout})", spec.commit);
    println!("  command   {}", spec.command);
    match &spec.client_report {
        Some(report) => println!("  exit code {} at {}", report.exit_code, report.finished_at),
        None => println!("  exit code not reported"),
    }
    println!("  sessions  {} observed by the operator", sessions.len());
    for session in sessions {
        println!(
            "    {} in {}, by {}, {} to {}",
            session.target,
            session.namespace,
            session.owner,
            session.started_at,
            session.ended_at.as_deref().unwrap_or("still running"),
        );
    }
}
