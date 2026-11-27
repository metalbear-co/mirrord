//! Handler for the `mirrord replay` command.
//!
//! A local session keeps the HTTP requests it received for the local application. This command
//! lists them and sends them to the application again, through the HTTP API of the session.
//!
//! The operator can also keep the split-queue messages that it routed to a session. The
//! `messages` and `send-message` subcommands list those and publish one to the session's output
//! queue again, through the operator's `queuereplays` resource.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use http::{Request, StatusCode};
use kube::{
    Api, Client,
    config::{KubeConfigOptions, Kubeconfig},
};
use mirrord_kube::api::kubernetes::create_kube_config;
use mirrord_operator::{
    client::add_baggage_header,
    crd::{MirrordOperatorCrd, NewOperatorFeature, OPERATOR_STATUS_NAME},
};
use mirrord_session_monitor_client::{
    SessionClient, SessionConnection, SessionError, connect_to_session, session_endpoints,
    sessions_dir,
};
use mirrord_session_monitor_protocol::{
    CapturedBody, CapturedRequest, CapturedRequestSummary, REPLAY_REQUESTS_ROUTE, ReplayOptions,
    ReplayRun, SessionInfo,
};
use prettytable::{Table, format, row};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{
    CliResult,
    config::{ReplayArgs, ReplayFormat, ReplaySubcommand},
    error::CliError,
    session::load_sessions,
};

/// The entrypoint for the `replay` command.
#[mirrord_macros::boxed_future]
pub(crate) async fn replay_command(args: ReplayArgs) -> CliResult<()> {
    let session = local_session(args.session_id.as_deref()).await?;
    let client = &session.client;

    match args.command {
        ReplaySubcommand::List => {
            let requests: Vec<CapturedRequestSummary> = get(client, REPLAY_REQUESTS_ROUTE).await?;

            match args.format {
                ReplayFormat::Json => print_json(&requests)?,
                ReplayFormat::Pretty => print_list(&requests),
            }
        }

        ReplaySubcommand::Show { id } => {
            let request: CapturedRequest =
                get(client, format!("{REPLAY_REQUESTS_ROUTE}/{id}")).await?;

            match args.format {
                ReplayFormat::Json => print_json(&request)?,
                ReplayFormat::Pretty => print_request(&request),
            }
        }

        ReplaySubcommand::Send {
            id,
            headers,
            remove_headers,
        } => {
            let mut options = ReplayOptions::default();
            for header in headers {
                let (name, value) = header.split_once(':').ok_or_else(|| {
                    CliError::Replay(format!("header `{header}` is not in `NAME: VALUE` format"))
                })?;
                options
                    .headers
                    .insert(header_key(name), Some(value.trim().to_owned()));
            }
            for name in remove_headers {
                options.headers.insert(header_key(&name), None);
            }

            let response = client
                .post(format!("{REPLAY_REQUESTS_ROUTE}/{id}/replay"))
                .json(&options)
                .send()
                .await
                .map_err(replay_error)?;
            let run: ReplayRun = response.json().await.map_err(replay_error)?;

            match args.format {
                ReplayFormat::Json => print_json(&run)?,
                ReplayFormat::Pretty => {
                    // The session can drop the request while the replay runs, to make room for
                    // newer ones. The response of the replay is still worth printing.
                    let original =
                        get::<Vec<CapturedRequestSummary>>(client, REPLAY_REQUESTS_ROUTE)
                            .await
                            .ok()
                            .and_then(|requests| {
                                requests.into_iter().find(|request| request.id == id)
                            })
                            .and_then(|request| request.status.zip(request.duration_ms));

                    print_run(id, original, &run);
                }
            }
        }

        ReplaySubcommand::Messages => {
            let messages = list_queue_messages(&session.info).await?;

            match args.format {
                ReplayFormat::Json => print_json(&messages)?,
                ReplayFormat::Pretty => print_messages(&messages),
            }
        }

        ReplaySubcommand::SendMessage { id } => {
            let replayed = replay_queue_message(&session.info, id).await?;

            match args.format {
                ReplayFormat::Json => print_json(&replayed)?,
                ReplayFormat::Pretty => println!(
                    "Published message {id} to the session's queue again{} (replay {}).",
                    replayed
                        .message_id
                        .map(|message_id| format!(" as {message_id}"))
                        .unwrap_or_default(),
                    replayed.replay_count,
                ),
            }
        }
    }

    Ok(())
}

/// Returns the local session `session_id`, or the only running local session when `session_id`
/// is `None`.
async fn local_session(session_id: Option<&str>) -> CliResult<SessionConnection> {
    if let Some(session_id) = session_id {
        let sessions_dir = sessions_dir()
            .ok_or_else(|| CliError::Replay("could not determine home directory".to_owned()))?;

        let (_, endpoint) = session_endpoints(&sessions_dir)
            .into_iter()
            .find(|(id, _)| id == session_id)
            .ok_or_else(|| CliError::Replay(format!("no local session has id `{session_id}`")))?;

        return connect_to_session(&endpoint.sentinel_path)
            .await
            .map_err(replay_error);
    }

    let mut sessions = load_sessions().await?;
    match sessions.len() {
        0 => Err(CliError::Replay(
            "no local mirrord session is running".to_owned(),
        )),
        1 => Ok(sessions.swap_remove(0)),
        _ => {
            let ids = sessions
                .iter()
                .map(|session| session.info.session_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");

            Err(CliError::Replay(format!(
                "several local sessions are running, pass `--session-id` with one of: {ids}"
            )))
        }
    }
}

/// Path of the operator's `queuereplays` resource.
const QUEUE_REPLAYS_PATH: &str = "/apis/operator.metalbear.co/v1/queuereplays";

/// A queue message that the operator kept for a session, as the operator serves it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct KeptMessage {
    id: u64,
    captured_at: String,
    event: serde_json::Value,
    body_size: usize,
    body_preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    not_replayable: Option<String>,
    replay_count: u32,
}

/// The operator's answer to publishing a kept queue message again.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ReplayedMessage {
    id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    replay_count: u32,
}

/// Lists the queue messages that the operator kept for the session `info`.
pub(crate) async fn list_queue_messages(info: &SessionInfo) -> CliResult<Vec<KeptMessage>> {
    let path = format!(
        "{QUEUE_REPLAYS_PATH}?session_key={}",
        url_encoded(session_key(info)?)
    );

    let request = Request::get(path)
        .body(Vec::new())
        .map_err(|error| CliError::Replay(error.to_string()))?;

    operator_request(info, request).await
}

/// Publishes the queue message `id` that the operator kept for the session `info` to the
/// session's output queue again.
pub(crate) async fn replay_queue_message(
    info: &SessionInfo,
    id: u64,
) -> CliResult<ReplayedMessage> {
    let body = serde_json::to_vec(&serde_json::json!({
        "session_key": session_key(info)?,
        "id": id,
    }))?;
    let request = Request::post(QUEUE_REPLAYS_PATH)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(body)
        .map_err(|error| CliError::Replay(error.to_string()))?;

    operator_request(info, request).await
}

/// The operator session key of the session `info`. Only sessions through the operator have one.
fn session_key(info: &SessionInfo) -> CliResult<&str> {
    info.key
        .as_deref()
        .filter(|_| info.is_operator)
        .ok_or_else(|| {
            CliError::Replay(
                "this session doesn't run through the operator, and only the operator keeps \
                 queue messages"
                    .to_owned(),
            )
        })
}

fn url_encoded(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

/// How long building a kube client may take. Bounds an auth-exec plugin that blocks on an expired
/// credential.
const CLIENT_BUILD_TIMEOUT: Duration = Duration::from_secs(15);

/// Kube clients by kubeconfig and context.
type OperatorClients = HashMap<(Option<String>, Option<String>), Client>;

/// Kube clients for the operators that serve `queuereplays`, by kubeconfig and context, so that
/// the Replay tab of `mirrord ui` doesn't run the auth plugin of the kubeconfig on every poll.
static OPERATOR_CLIENTS: LazyLock<Mutex<OperatorClients>> = LazyLock::new(Default::default);

/// Sends `request` to the operator of the session `info`, with the kubeconfig, context and
/// baggage that the session uses, and returns the JSON response.
async fn operator_request<T: DeserializeOwned>(
    info: &SessionInfo,
    request: Request<Vec<u8>>,
) -> CliResult<T> {
    let client = operator_client(info).await?;
    let text = client
        .request_text(request)
        .await
        .map_err(|error| match error {
            // The operator says why it refused, for example that queue replay is off.
            kube::Error::Api(response) => CliError::Replay(response.message),
            error => CliError::Replay(error.to_string()),
        })?;

    serde_json::from_str(&text).map_err(|error| CliError::Replay(error.to_string()))
}

/// A client for the operator of the session `info`, once the operator says that it serves
/// `queuereplays`.
async fn operator_client(info: &SessionInfo) -> CliResult<Client> {
    let key = (info.kubeconfig.clone(), info.context.clone());
    if let Some(client) = lock_clients().get(&key).cloned() {
        return Ok(client);
    }

    let client = tokio::time::timeout(CLIENT_BUILD_TIMEOUT, build_client(info))
        .await
        .map_err(|_| {
            CliError::Replay(format!(
                "connecting to the cluster took longer than {} seconds",
                CLIENT_BUILD_TIMEOUT.as_secs()
            ))
        })??;

    let operator = Api::<MirrordOperatorCrd>::all(client.clone())
        .get(OPERATOR_STATUS_NAME)
        .await
        .map_err(|error| CliError::Replay(format!("couldn't read the operator: {error}")))?;
    operator
        .spec
        .require_feature(NewOperatorFeature::QueueReplay)?;

    lock_clients().insert(key, client.clone());
    Ok(client)
}

async fn build_client(info: &SessionInfo) -> CliResult<Client> {
    let error = |error: &dyn std::fmt::Display| CliError::Replay(error.to_string());
    let options = KubeConfigOptions {
        context: info.context.clone(),
        ..Default::default()
    };

    // The session read this kubeconfig, which can list several files, like `KUBECONFIG`.
    let mut config = match &info.kubeconfig {
        Some(paths) => {
            let mut files = std::env::split_paths(paths)
                .filter(|path| !path.as_os_str().is_empty())
                .map(Kubeconfig::read_from);
            let first = files
                .next()
                .ok_or_else(|| CliError::Replay("the session's kubeconfig is empty".to_owned()))?
                .map_err(|e| error(&e))?;
            let merged = files
                .try_fold(first, |merged, next| merged.merge(next?))
                .map_err(|e| error(&e))?;

            kube::Config::from_custom_kubeconfig(merged, &options)
                .await
                .map_err(|e| error(&e))?
        }
        None => create_kube_config(None, None::<&str>, info.context.clone())
            .await
            .map_err(|e| error(&e))?,
    };

    if let Some(accept) = info
        .config
        .get("accept_invalid_certificates")
        .and_then(Value::as_bool)
    {
        config.accept_invalid_certs = accept;
    }
    add_baggage_header(
        &mut config,
        info.config.get("baggage").and_then(Value::as_str),
    )
    .map_err(|e| error(&e))?;

    Client::try_from(config).map_err(|e| error(&e))
}

fn lock_clients() -> MutexGuard<'static, OperatorClients> {
    OPERATOR_CLIENTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn print_messages(messages: &[KeptMessage]) {
    if messages.is_empty() {
        println!(
            "The operator keeps no queue messages for this session yet. It keeps the SQS and \
            Kafka messages that it routes to the session, for an hour."
        );
        return;
    }

    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_CLEAN);
    table.add_row(row!["ID", "QUEUE", "MESSAGE", "SIZE", "BODY", "REPLAYS"]);
    for message in messages {
        let field = |kind: &str, name: &str| message.event.get(kind)?.get(name).cloned();
        let queue = field("queue_message", "queue_name")
            .or_else(|| field("kafka_message", "topic"))
            .and_then(|name| name.as_str().map(str::to_owned));
        let queue = queue.as_deref().unwrap_or("-");
        let message_id = field("queue_message", "message_id")
            .and_then(|id| id.as_str().map(str::to_owned))
            .or_else(|| {
                let partition = field("kafka_message", "partition")?.as_i64()?;
                let offset = field("kafka_message", "offset")?.as_i64()?;
                Some(format!("{partition}@{offset}"))
            })
            .unwrap_or_else(|| "-".to_owned());
        let preview: String = message.body_preview.chars().take(40).collect();
        let replays = match &message.not_replayable {
            Some(reason) => format!("can't be replayed: {reason}"),
            None => message.replay_count.to_string(),
        };

        table.add_row(row![
            message.id,
            printable(queue),
            printable(&message_id),
            format!("{} B", message.body_size),
            printable(&preview.replace('\n', " ")),
            replays
        ]);
    }

    table.printstd();
}

/// Returns `name` in the form that identifies a header in [`ReplayOptions`]: trimmed and in
/// lowercase. Header names are not case-sensitive, so the same header in another case replaces
/// the earlier one.
fn header_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

async fn get<T: DeserializeOwned>(client: &SessionClient, path: impl Into<String>) -> CliResult<T> {
    let response = client.get(path).send().await.map_err(replay_error)?;

    response.json().await.map_err(replay_error)
}

fn replay_error(error: SessionError) -> CliError {
    match error {
        // The session API of a mirrord version without replay answers the replay routes with an
        // empty 404.
        SessionError::Upstream { status, body }
            if status == StatusCode::NOT_FOUND && body.is_empty() =>
        {
            CliError::Replay(
                "the session was started by a mirrord version that doesn't keep requests"
                    .to_owned(),
            )
        }

        // The session says why it refused, for example that the request can't be replayed.
        SessionError::Upstream { body, .. } if !body.is_empty() => CliError::Replay(body),

        error => CliError::Replay(error.to_string()),
    }
}

fn print_json<T: Serialize>(value: &T) -> CliResult<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn print_list(requests: &[CapturedRequestSummary]) {
    if requests.is_empty() {
        println!(
            "The session has no requests yet. It keeps the HTTP requests that it steals or \
            mirrors, except health probes."
        );
        return;
    }

    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_CLEAN);
    table.add_row(row![
        "ID", "METHOD", "PATH", "STATUS", "TOOK", "AGE", "REPLAYS"
    ]);
    for request in requests {
        let replays = match &request.not_replayable {
            Some(reason) => format!("can't be replayed: {reason}"),
            None => request.replay_count.to_string(),
        };
        table.add_row(row![
            request.id,
            printable(&request.method),
            printable(&request.path),
            request
                .status
                .map_or_else(|| "-".to_owned(), |status| status.to_string()),
            request
                .duration_ms
                .map_or_else(|| "-".to_owned(), |millis| format!("{millis} ms")),
            age(request.captured_at_ms),
            replays
        ]);
    }

    table.printstd();
}

fn print_request(request: &CapturedRequest) {
    let summary = &request.summary;
    println!(
        "{} {} {}   (request {}, {} on port {}, {} ago)",
        printable(&summary.method),
        printable(&summary.path),
        request.version,
        summary.id,
        summary.mode,
        summary.port,
        age(summary.captured_at_ms),
    );
    print_headers_and_body(&request.headers, &request.body);

    match &request.response {
        Some(response) => println!(
            "\nResponse: {}",
            status_and_duration(response.status, response.duration_ms)
        ),
        None => println!("\nResponse: none, the application hasn't answered"),
    }

    for run in &request.replays {
        println!(
            "Replay {}: {}",
            run.number,
            status_and_duration(run.response.status, run.response.duration_ms)
        );
    }

    if let Some(reason) = &summary.not_replayable {
        println!("Can't be replayed: {reason}");
    }
}

/// Prints the response to the replay `run` of request `id`, next to the status and duration in
/// milliseconds of the response to the `original` request.
fn print_run(id: u64, original: Option<(u16, u64)>, run: &ReplayRun) {
    let original = original.map_or_else(
        || "no response".to_owned(),
        |(status, duration_ms)| status_and_duration(status, duration_ms),
    );

    println!(
        "Replay {} of request {id}: {}   (original: {original})",
        run.number,
        status_and_duration(run.response.status, run.response.duration_ms),
    );
    print_headers_and_body(&run.response.headers, &run.body);
}

fn print_headers_and_body(headers: &[(String, String)], body: &CapturedBody) {
    for (name, value) in headers {
        println!("{}: {}", printable(name), printable(value));
    }

    let text = printable(&body.text);
    match (body.size, body.truncated) {
        (0, false) => {}
        (0, true) => println!("\n(no body arrived in time)"),
        (_, false) => println!("\n{text}"),
        (_, true) => println!("\n{text}\n(only the start of the body is shown)"),
    }
}

/// Returns `text` with its control characters escaped, except tabs and line breaks, and with the
/// characters that change the direction of text escaped.
///
/// The text comes from HTTP traffic, which anyone who can reach the target can send. A terminal
/// would run the escape sequences in it, and would show the text after a change of direction in
/// another order than it has.
fn printable(text: &str) -> String {
    let mut printable = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let keep = match c {
            '\n' | '\t' => true,
            // On its own, a carriage return moves the cursor back over what is already printed.
            '\r' => chars.peek() == Some(&'\n'),
            // The embeddings, overrides and isolates of the Unicode bidirectional algorithm.
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => false,
            c => !c.is_control(),
        };

        if keep {
            printable.push(c);
        } else {
            printable.extend(c.escape_default());
        }
    }

    printable
}

fn status_and_duration(status: u16, duration_ms: u64) -> String {
    format!("{status} in {duration_ms} ms")
}

/// How long ago `unix_millis` was, rounded down to whole seconds.
fn age(unix_millis: u64) -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH + Duration::from_millis(unix_millis))
        .unwrap_or_default();

    humantime::format_duration(Duration::from_secs(elapsed.as_secs())).to_string()
}
