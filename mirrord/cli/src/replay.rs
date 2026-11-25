//! Handler for the `mirrord replay` command.
//!
//! A local session keeps the HTTP requests it received for the local application. This command
//! lists them and sends them to the application again, through the HTTP API of the session.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::StatusCode;
use mirrord_session_monitor_client::{
    SessionClient, SessionError, session_endpoints, sessions_dir,
};
use mirrord_session_monitor_protocol::{
    CapturedBody, CapturedRequest, CapturedRequestSummary, REPLAY_REQUESTS_ROUTE, ReplayOptions,
    ReplayRun,
};
use prettytable::{Table, format, row};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    CliResult,
    config::{ReplayArgs, ReplayFormat, ReplaySubcommand},
    error::CliError,
    session::load_sessions,
};

/// The entrypoint for the `replay` command.
#[mirrord_macros::boxed_future]
pub(crate) async fn replay_command(args: ReplayArgs) -> CliResult<()> {
    let client = session_client(args.session_id.as_deref()).await?;

    match args.command {
        ReplaySubcommand::List => {
            let requests: Vec<CapturedRequestSummary> = get(&client, REPLAY_REQUESTS_ROUTE).await?;

            match args.format {
                ReplayFormat::Json => print_json(&requests)?,
                ReplayFormat::Pretty => print_list(&requests),
            }
        }

        ReplaySubcommand::Show { id } => {
            let request: CapturedRequest =
                get(&client, format!("{REPLAY_REQUESTS_ROUTE}/{id}")).await?;

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
                        get::<Vec<CapturedRequestSummary>>(&client, REPLAY_REQUESTS_ROUTE)
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
    }

    Ok(())
}

/// Returns a client for the local session `session_id`, or for the only running local session
/// when `session_id` is `None`.
async fn session_client(session_id: Option<&str>) -> CliResult<SessionClient> {
    if let Some(session_id) = session_id {
        let sessions_dir = sessions_dir()
            .ok_or_else(|| CliError::Replay("could not determine home directory".to_owned()))?;

        return session_endpoints(&sessions_dir)
            .into_iter()
            .find(|(id, _)| id == session_id)
            .map(|(_, endpoint)| SessionClient::new(endpoint))
            .ok_or_else(|| CliError::Replay(format!("no local session has id `{session_id}`")));
    }

    let mut sessions = load_sessions().await?;
    match sessions.len() {
        0 => Err(CliError::Replay(
            "no local mirrord session is running".to_owned(),
        )),
        1 => Ok(sessions.swap_remove(0).client),
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
            request.method,
            request.path,
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
        summary.method,
        summary.path,
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
        println!("{name}: {value}");
    }

    match (body.size, body.truncated) {
        (0, false) => {}
        (0, true) => println!("\n(no body arrived in time)"),
        (_, false) => println!("\n{}", body.text),
        (_, true) => println!("\n{}\n(only the start of the body is shown)", body.text),
    }
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
