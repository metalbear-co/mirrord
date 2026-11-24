//! `mirrord verify`: runs a test command and keeps a record of the run on the cluster.

use std::{ops::Not, path::PathBuf, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{SecondsFormat, Utc};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Api, Client,
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
            ATTESTATION_PAYLOAD_TYPE, DsseEnvelope, MirrordVerification, MirrordVerificationSpec,
            VERIFICATION_COMMIT_LABEL, VERIFICATION_KEY_PREFIX, VerificationCheck,
            VerificationClientReport, VerificationPhase, pae, verification_key,
        },
    },
    types::{OPERATOR_OWNERSHIP_LABEL, isolation_marker},
};
use mirrord_progress::NullProgress;
use rand::distr::{Alphanumeric, SampleString};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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

/// How long `run` waits for the operator to finish a record's checks.
const CHECKS_TIMEOUT: Duration = Duration::from_secs(180);

/// Exit code of `check` when no verification of the commit passed.
const EXIT_NOT_VERIFIED: i32 = 1;

/// Exit code of `check` when the records can't be read.
const EXIT_NO_RECORDS: i32 = 2;

/// Namespace the operator is assumed to run in when it does not report one.
const DEFAULT_OPERATOR_NAMESPACE: &str = "mirrord";

/// ConfigMap in the operator's namespace that publishes the key it signs records with.
const PUBLIC_KEY_CONFIG_MAP: &str = "mirrord-verification-public-key";

/// The API for verification records, and where their signing key is published.
struct VerifyApi {
    records: Api<MirrordVerification>,
    client: Client,
    operator_namespace: String,
}

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
    let checks = match &args.checks {
        Some(path) => read_checks(path)?,
        None => Vec::new(),
    };
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
        checks,
    };
    let has_checks = spec.checks.is_empty().not();

    let api = match create_record(args.config_file, &key, spec).await {
        Ok(api) => Some(api),
        // The operator runs the checks, so without a record there is nothing to wait for.
        Err(error) if has_checks => return Err(error),
        Err(error) => {
            eprintln!("mirrord verify: running without a verification record: {error}");
            None
        }
    };

    let (program, program_args) = args
        .command
        .split_first()
        .ok_or(VerifyError::EmptyCommand)?;
    let mut child = resolve_tokio_command(program)
        .args(program_args)
        .env(MIRRORD_ENV_KEY, &key)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| VerifyError::CommandNotRun {
            command: command.clone(),
            error,
        })?;

    let exit_code = match &api {
        Some(api) if has_checks => {
            let deadline = Instant::now() + CHECKS_TIMEOUT;
            loop {
                if let Some(status) = child.try_wait().map_err(VerifyError::Wait)? {
                    eprintln!(
                        "mirrord verify: the command exited before the operator finished its checks"
                    );
                    // A command killed by a signal has no exit code.
                    break status.code().filter(|code| *code != 0).unwrap_or(1);
                }
                if checks_finished(&api.records, &key).await {
                    // The command only had to stay up for the checks, so stopping it is a
                    // success.
                    let _ = child.kill().await;
                    break 0;
                }
                if Instant::now() >= deadline {
                    eprintln!(
                        "mirrord verify: the operator did not finish its checks in {} seconds",
                        CHECKS_TIMEOUT.as_secs()
                    );
                    let _ = child.kill().await;
                    break 1;
                }
                sleep(Duration::from_secs(1)).await;
            }
        }
        _ => child
            .wait()
            .await
            .map_err(VerifyError::Wait)?
            .code()
            .unwrap_or(1),
    };

    let mut verified = has_checks.not();
    if let Some(api) = api {
        match report(&api.records, &key, exit_code).await {
            Ok(record) => {
                print_record(&record);
                print_signature(&api, &record).await;
                verified = verified || phase(&record) == VerificationPhase::Passed;
            }
            Err(error) => {
                eprintln!("mirrord verify: the verification record `{key}` is incomplete: {error}")
            }
        }
    }

    // With checks the operator's verdict is the result, since the command was only stopped.
    let exit_code = if exit_code == 0 && verified.not() {
        EXIT_NOT_VERIFIED
    } else {
        exit_code
    };
    std::process::exit(exit_code)
}

async fn show(args: VerifyLookupArgs) -> CliResult<()> {
    let api = verify_api(args.config_file).await?;
    let records = find(&api.records, &args.reference).await?;
    if records.is_empty() {
        return Err(VerifyError::NoRecord(args.reference).into());
    }

    for record in &records {
        print_record(record);
        print_signature(&api, record).await;
    }
    Ok(())
}

/// Passes only for a record whose signed statement says that this commit passed.
async fn check(args: VerifyLookupArgs) -> CliResult<()> {
    if is_full_commit(&args.reference).not() {
        return Err(VerifyError::NotAFullCommit(args.reference).into());
    }

    let found = match verify_api(args.config_file).await {
        Ok(api) => find(&api.records, &args.reference)
            .await
            .map(|records| (api, records)),
        Err(error) => Err(error),
    };
    let (api, records) = match found {
        Ok(found) => found,
        Err(error) => {
            eprintln!("mirrord verify: can't read verification records: {error}");
            std::process::exit(EXIT_NO_RECORDS)
        }
    };

    let public_key = match public_key(&api).await {
        Ok(public_key) => public_key,
        Err(error) => {
            eprintln!("mirrord verify: can't read the operator's signing key: {error}");
            std::process::exit(EXIT_NO_RECORDS)
        }
    };

    for record in &records {
        if signed_statement(record, &public_key)
            .is_ok_and(|statement| statement_passes(&statement, &args.reference))
        {
            print_record(record);
            println!("  signature valid, signed by the operator");
            return Ok(());
        }
    }

    if records.is_empty() {
        eprintln!(
            "mirrord verify: no verification record for commit {}",
            args.reference
        );
    }
    for record in &records {
        print_record(record);
        print_signature(&api, record).await;
    }
    std::process::exit(EXIT_NOT_VERIFIED)
}

/// The API for verification records, when the operator of the configured cluster keeps them.
async fn verify_api(config_file: Option<PathBuf>) -> CliResult<VerifyApi> {
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

    let client = operator_api.client().clone();
    let operator_namespace = operator_api
        .operator()
        .spec
        .operator_namespace
        .clone()
        .unwrap_or_else(|| DEFAULT_OPERATOR_NAMESPACE.to_owned());

    Ok(VerifyApi {
        records: Api::all(client.clone()),
        client,
        operator_namespace,
    })
}

async fn create_record(
    config_file: Option<PathBuf>,
    key: &str,
    spec: MirrordVerificationSpec,
) -> CliResult<VerifyApi> {
    let api = verify_api(config_file).await?;

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
    api.records
        .create(&PostParams::default(), &record)
        .await
        .map_err(VerifyError::Kube)?;

    Ok(api)
}

/// The checks in a YAML or JSON file: a list of [`VerificationCheck`].
fn read_checks(path: &PathBuf) -> Result<Vec<VerificationCheck>, VerifyError> {
    let content = std::fs::read_to_string(path).map_err(|error| VerifyError::ChecksFile {
        path: path.clone(),
        reason: error.to_string(),
    })?;

    serde_yaml::from_str(&content).map_err(|error| VerifyError::ChecksFile {
        path: path.clone(),
        reason: error.to_string(),
    })
}

/// Whether the operator has a result for every check of the record.
async fn checks_finished(api: &Api<MirrordVerification>, key: &str) -> bool {
    api.get(key).await.is_ok_and(|record| {
        record
            .status
            .is_some_and(|status| status.checks.len() >= record.spec.checks.len())
    })
}

/// Adds the command's exit code to the record, then waits for the operator's verdict and its
/// signature.
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
    while decided(&record).not() && Instant::now() < deadline {
        sleep(Duration::from_secs(1)).await;
        record = api.get(key).await?;
    }

    Ok(record)
}

/// Whether the operator has set a verdict and signed it.
fn decided(record: &MirrordVerification) -> bool {
    record.status.as_ref().is_some_and(|status| {
        status.phase != VerificationPhase::Running && status.attestation.is_some()
    })
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

/// The key the operator signs records with, as it publishes it in its namespace.
async fn public_key(api: &VerifyApi) -> Result<Vec<u8>, VerifyError> {
    let config_map = Api::<ConfigMap>::namespaced(api.client.clone(), &api.operator_namespace)
        .get(PUBLIC_KEY_CONFIG_MAP)
        .await?;
    let encoded = config_map
        .data
        .and_then(|mut data| data.remove("publicKey"))
        .ok_or(VerifyError::Signature("the operator published no key"))?;

    STANDARD
        .decode(encoded)
        .map_err(|_| VerifyError::Signature("the operator's published key is not base64"))
}

/// The statement the operator signed for the record, after checking its signature against the
/// operator's key.
fn signed_statement(record: &MirrordVerification, public_key: &[u8]) -> Result<Value, VerifyError> {
    let attestation = record
        .status
        .as_ref()
        .and_then(|status| status.attestation.as_deref())
        .ok_or(VerifyError::Signature("the record is not signed"))?;
    let envelope: DsseEnvelope = serde_json::from_str(attestation)
        .map_err(|_| VerifyError::Signature("the signed statement can't be read"))?;
    if envelope.payload_type != ATTESTATION_PAYLOAD_TYPE {
        return Err(VerifyError::Signature(
            "the signed statement has an unknown type",
        ));
    }
    let payload = STANDARD
        .decode(&envelope.payload)
        .map_err(|_| VerifyError::Signature("the signed statement can't be read"))?;

    let keyid = hex::encode(Sha256::digest(public_key));
    let signature = envelope
        .signatures
        .iter()
        .find(|signature| signature.keyid == keyid)
        .and_then(|signature| STANDARD.decode(&signature.sig).ok())
        .ok_or(VerifyError::Signature(
            "the record is not signed with the operator's key",
        ))?;
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&pae(ATTESTATION_PAYLOAD_TYPE, &payload), &signature)
        .map_err(|_| VerifyError::Signature("the signature does not match the record"))?;

    serde_json::from_slice(&payload)
        .map_err(|_| VerifyError::Signature("the signed statement can't be read"))
}

/// Whether a signed statement says that the commit passed.
fn statement_passes(statement: &Value, commit: &str) -> bool {
    let subject_is_commit = statement
        .get("subject")
        .and_then(Value::as_array)
        .is_some_and(|subjects| {
            subjects.iter().any(|subject| {
                subject.pointer("/digest/gitCommit").and_then(Value::as_str) == Some(commit)
            })
        });

    subject_is_commit
        && statement
            .pointer("/predicate/phase")
            .and_then(Value::as_str)
            == Some("Passed")
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
    let status = record.status.clone().unwrap_or_default();
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
    println!(
        "  sessions  {} observed by the operator",
        status.sessions.len()
    );
    for session in &status.sessions {
        println!(
            "    {} in {}, by {}, {} to {}",
            session.target,
            session.namespace,
            session.owner,
            session.started_at,
            session.ended_at.as_deref().unwrap_or("still running"),
        );
    }
    if spec.checks.is_empty().not() {
        println!(
            "  checks    {} of {} passed, sent and judged by the operator",
            status.checks.iter().filter(|check| check.passed).count(),
            spec.checks.len()
        );
    }
    for check in &status.checks {
        let mark = if check.passed { "pass" } else { "FAIL" };
        let deployed = match (&check.body_sha256, &check.deployed_body_sha256) {
            (Some(keyed), Some(deployed)) if keyed == deployed => {
                "same answer as the deployed workload"
            }
            (Some(_), Some(_)) => "differs from the deployed workload's answer",
            _ => "no answer from the deployed workload to compare",
        };
        println!("    {mark}  {}: {}, {deployed}", check.name, check.detail);
    }
}

/// Says whether the record carries a valid signature of the operator, and whether the record
/// still says what the operator signed.
async fn print_signature(api: &VerifyApi, record: &MirrordVerification) {
    let statement = match public_key(api).await {
        Ok(public_key) => signed_statement(record, &public_key),
        Err(error) => Err(error),
    };
    match statement {
        Ok(statement) if matches_statement(record, &statement) => {
            println!("  signature valid, signed by the operator")
        }
        Ok(statement) => println!(
            "  CHANGED AFTER SIGNING: the operator signed the verdict {}, the record now says {}",
            statement
                .pointer("/predicate/phase")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            phase(record)
        ),
        Err(error) => println!("  signature not verified: {error}"),
    }
}

/// Whether the record's verdict and check results are the ones in its signed statement.
fn matches_statement(record: &MirrordVerification, statement: &Value) -> bool {
    let status = record.status.clone().unwrap_or_default();
    let signed_phase = statement
        .pointer("/predicate/phase")
        .and_then(Value::as_str);
    let signed_checks = statement.pointer("/predicate/checks");

    signed_phase == Some(status.phase.to_string().as_str())
        && signed_checks == serde_json::to_value(&status.checks).ok().as_ref()
}
