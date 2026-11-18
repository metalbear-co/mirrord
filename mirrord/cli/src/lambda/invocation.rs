//! Invocation requests on the invocation port, turned back into invocations for the local runtime,
//! and the runtime's answers turned back into their responses.
//!
//! The hub delivers two shapes, told apart by the `kind` in `X-Mirrord-Lambda`:
//!
//! - `kind=invoke` (SQS, direct invocation) follows the contract of the AWS Runtime Interface
//!   Emulator's invoke endpoint: the event is the body, and the metadata comes in `X-Amzn-*` and
//!   `X-Amz-*` headers. The response is the invocation result. A handler error is a `200` with
//!   `X-Amz-Function-Error` and the runtime's error document as the body.
//! - `kind=function-url` (Function URL, API Gateway HTTP API) is the original HTTP request, so the
//!   session's HTTP filters see what they'd see on any other target. The adapter rebuilds the
//!   payload format 2.0 event from it and from `X-Mirrord-Lambda-Context`, and answers with the
//!   HTTP response the handler's result describes. A handler error is a `502` with
//!   `X-Amz-Function-Error`.
//!
//! A request without `X-Mirrord-Lambda` is taken as the emulator shape when it's a `POST` to the
//! emulator's invoke path, and as a Function URL request otherwise. That lets a developer drive the
//! adapter with `curl` alone.

use std::{collections::BTreeMap, ops::Not, sync::Arc, time::Duration};

use axum::{
    body::{Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, prelude::BASE64_STANDARD};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    MAX_PAYLOAD,
    runtime_api::{FunctionError, Invocation, Invocations, Outcome, deadline},
};

/// Metadata the hub adds to every invocation request.
const MIRRORD_LAMBDA: &str = "x-mirrord-lambda";

/// A Function URL invocation's `requestContext` and the Runtime API metadata headers, which have
/// no place in the rebuilt HTTP request.
const MIRRORD_LAMBDA_CONTEXT: &str = "x-mirrord-lambda-context";

/// The `X-Mirrord-Lambda` version this adapter understands.
const VERSION: &str = "1";

/// The remaining time of an invocation that doesn't say, which is Lambda's longest timeout.
const DEFAULT_REMAINING: Duration = Duration::from_mins(15);

const FUNCTION_ERROR: HeaderName = HeaderName::from_static("x-amz-function-error");

#[derive(Debug, Clone, Copy)]
enum Shape {
    Invoke,
    FunctionUrl,
}

/// The parsed `X-Mirrord-Lambda` header: `v=1; kind=invoke; remaining-ms=1234; ...`.
#[derive(Debug, Default)]
struct MirrordLambda {
    kind: Option<String>,
    request_id: Option<String>,
    remaining: Option<Duration>,
    function_arn: Option<String>,
}

impl MirrordLambda {
    fn parse(value: &str) -> Result<Self, String> {
        let mut parsed = Self::default();
        let mut version = None;

        for (key, value) in value
            .split(';')
            .filter_map(|pair| pair.split_once('='))
            .map(|(key, value)| (key.trim(), value.trim()))
        {
            match key {
                "v" => version = Some(value),
                "kind" => parsed.kind = Some(value.to_owned()),
                "request-id" => parsed.request_id = Some(value.to_owned()),
                "remaining-ms" => {
                    let remaining = value
                        .parse()
                        .map_err(|_| format!("invalid remaining-ms `{value}`"))?;
                    parsed.remaining = Some(Duration::from_millis(remaining));
                }
                "function-arn" => parsed.function_arn = Some(value.to_owned()),
                _ => {}
            }
        }

        match version {
            Some(VERSION) => Ok(parsed),
            Some(other) => Err(format!("unsupported version `{other}`")),
            None => Err("no version".to_owned()),
        }
    }
}

/// `X-Mirrord-Lambda-Context`, base64 encoded JSON.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FunctionUrlContext {
    request_context: Option<Value>,
    /// The Runtime API metadata headers of the original invocation, by name.
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

impl FunctionUrlContext {
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }
}

/// Serves one invocation request: queues the invocation for the local runtime and answers with its
/// outcome.
pub(super) async fn handle(
    State(invocations): State<Arc<Invocations>>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_PAYLOAD).await {
        Ok(body) => body,
        Err(error) => {
            return bad_request(format!("failed to read the invocation body: {error}"));
        }
    };

    let mirrord_lambda = match parts
        .headers
        .get(MIRRORD_LAMBDA)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| "not valid text".to_owned())
                .and_then(MirrordLambda::parse)
        })
        .transpose()
    {
        Ok(mirrord_lambda) => mirrord_lambda,
        Err(error) => return bad_request(format!("invalid {MIRRORD_LAMBDA}: {error}")),
    };

    let shape = match mirrord_lambda
        .as_ref()
        .and_then(|header| header.kind.as_deref())
    {
        Some("invoke") => Shape::Invoke,
        Some("function-url") => Shape::FunctionUrl,
        Some(other) => return bad_request(format!("unsupported invocation kind `{other}`")),
        None if is_emulator_invoke(&parts.method, &parts.uri) => Shape::Invoke,
        None => Shape::FunctionUrl,
    };
    let mirrord_lambda = mirrord_lambda.unwrap_or_default();

    let invocation = match shape {
        Shape::Invoke => invoke_invocation(&parts.headers, mirrord_lambda, body),
        Shape::FunctionUrl => {
            match function_url_invocation(
                &parts.method,
                &parts.uri,
                &parts.headers,
                mirrord_lambda,
                body,
            ) {
                Ok(invocation) => invocation,
                Err(error) => return bad_request(error),
            }
        }
    };

    let request_id = invocation.request_id.clone();
    let deadline = invocation.deadline;
    tracing::debug!(request_id, ?shape, "Received an invocation");

    let outcome = match invocations.enqueue(invocation) {
        Ok(outcome) => match tokio::time::timeout_at(deadline.into(), outcome).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the local Lambda runtime adapter is shutting down",
                )
                    .into_response();
            }
            Err(_) => Outcome::Error(FunctionError::local_timeout(&request_id)),
        },
        Err(init_error) => Outcome::Error(init_error),
    };
    tracing::debug!(
        request_id,
        failed = matches!(outcome, Outcome::Error(_)),
        "Answering an invocation"
    );

    match shape {
        Shape::Invoke => invoke_response(outcome),
        Shape::FunctionUrl => function_url_response(outcome),
    }
}

/// The emulator's `POST /2015-03-31/functions/{function}/invocations`.
fn is_emulator_invoke(method: &Method, uri: &Uri) -> bool {
    let segments = uri.path().trim_matches('/').split('/').collect::<Vec<_>>();
    method == Method::POST
        && matches!(
            segments.as_slice(),
            ["2015-03-31", "functions", _, "invocations"]
        )
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn invoke_invocation(
    headers: &HeaderMap,
    mirrord_lambda: MirrordLambda,
    body: Bytes,
) -> Invocation {
    let request_id = header(headers, "x-amzn-requestid")
        .or(mirrord_lambda.request_id)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (deadline, deadline_epoch_ms) =
        deadline(mirrord_lambda.remaining.unwrap_or(DEFAULT_REMAINING));

    // The Invoke API carries the client context base64 encoded, the Runtime API as JSON.
    let client_context = header(headers, "x-amz-client-context").and_then(|encoded| {
        BASE64_STANDARD
            .decode(&encoded)
            .ok()
            .and_then(|decoded| String::from_utf8(decoded).ok())
            .or_else(|| {
                tracing::debug!(encoded, "Dropping a client context that isn't base64 text");
                None
            })
    });

    Invocation {
        request_id,
        deadline,
        deadline_epoch_ms,
        function_arn: mirrord_lambda
            .function_arn
            .unwrap_or_else(default_function_arn),
        trace_id: header(headers, "x-amzn-trace-id"),
        client_context,
        cognito_identity: header(headers, "x-amz-cognito-identity"),
        event: body,
    }
}

fn function_url_invocation(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    mirrord_lambda: MirrordLambda,
    body: Bytes,
) -> Result<Invocation, String> {
    let context = match header(headers, MIRRORD_LAMBDA_CONTEXT) {
        Some(encoded) => BASE64_STANDARD
            .decode(&encoded)
            .map_err(|error| format!("{MIRRORD_LAMBDA_CONTEXT} is not base64: {error}"))
            .and_then(|decoded| {
                serde_json::from_slice::<FunctionUrlContext>(&decoded)
                    .map_err(|error| format!("{MIRRORD_LAMBDA_CONTEXT} is not valid: {error}"))
            })?,
        None => FunctionUrlContext::default(),
    };

    let request_id = mirrord_lambda
        .request_id
        .or_else(|| context.header("Lambda-Runtime-Aws-Request-Id"))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let (deadline, deadline_epoch_ms) =
        deadline(mirrord_lambda.remaining.unwrap_or(DEFAULT_REMAINING));

    let event = function_url_event(method, uri, headers, &request_id, &context, &body);

    Ok(Invocation {
        deadline,
        deadline_epoch_ms,
        function_arn: context
            .header("Lambda-Runtime-Invoked-Function-Arn")
            .or(mirrord_lambda.function_arn)
            .unwrap_or_else(default_function_arn),
        trace_id: context.header("Lambda-Runtime-Trace-Id"),
        client_context: context.header("Lambda-Runtime-Client-Context"),
        cognito_identity: context.header("Lambda-Runtime-Cognito-Identity"),
        event: event.to_string().into(),
        request_id,
    })
}

/// The payload format 2.0 event Lambda builds for a Function URL request.
fn function_url_event(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    request_id: &str,
    context: &FunctionUrlContext,
    body: &Bytes,
) -> Value {
    let mut event_headers = BTreeMap::<String, String>::new();
    let mut cookies = Vec::new();
    for (name, value) in headers {
        let name = name.as_str();
        let value = String::from_utf8_lossy(value.as_bytes());
        if name == MIRRORD_LAMBDA || name == MIRRORD_LAMBDA_CONTEXT {
            continue;
        }
        // Lambda moves cookies out of the headers into their own list.
        if name == header::COOKIE.as_str() {
            cookies.extend(value.split(';').map(|cookie| cookie.trim().to_owned()));
            continue;
        }
        event_headers
            .entry(name.to_owned())
            .and_modify(|joined| {
                joined.push(',');
                joined.push_str(&value);
            })
            .or_insert_with(|| value.into_owned());
    }

    let raw_query = uri.query().unwrap_or_default();
    let mut query_parameters = BTreeMap::<String, String>::new();
    for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        query_parameters
            .entry(key.into_owned())
            .and_modify(|joined| {
                joined.push(',');
                joined.push_str(&value);
            })
            .or_insert_with(|| value.into_owned());
    }

    let request_context = context.request_context.clone().unwrap_or_else(|| {
        let time_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        json!({
            "accountId": "anonymous",
            "domainName": event_headers.get("host"),
            "http": {
                "method": method.as_str(),
                "path": uri.path(),
                "protocol": "HTTP/1.1",
                "sourceIp": "127.0.0.1",
                "userAgent": event_headers.get("user-agent"),
            },
            "requestId": request_id,
            "routeKey": "$default",
            "stage": "$default",
            "timeEpoch": time_epoch,
        })
    });

    let mut event = Map::new();
    event.insert("version".to_owned(), json!("2.0"));
    event.insert("routeKey".to_owned(), json!("$default"));
    event.insert("rawPath".to_owned(), json!(uri.path()));
    event.insert("rawQueryString".to_owned(), json!(raw_query));
    if cookies.is_empty().not() {
        event.insert("cookies".to_owned(), json!(cookies));
    }
    event.insert("headers".to_owned(), json!(event_headers));
    if query_parameters.is_empty().not() {
        event.insert("queryStringParameters".to_owned(), json!(query_parameters));
    }
    event.insert("requestContext".to_owned(), request_context);

    // Text stays text; anything else is base64, which the handler decodes when
    // `isBase64Encoded` says so.
    let (body, is_base64_encoded) = match std::str::from_utf8(body) {
        Ok(text) => (text.to_owned(), false),
        Err(_) => (BASE64_STANDARD.encode(body), true),
    };
    if body.is_empty().not() {
        event.insert("body".to_owned(), json!(body));
    }
    event.insert("isBase64Encoded".to_owned(), json!(is_base64_encoded));

    Value::Object(event)
}

/// The ARN for an invocation that doesn't carry one, built like Lambda's from the function's
/// environment, which the hub serves.
fn default_function_arn() -> String {
    let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_owned());
    let function =
        std::env::var("AWS_LAMBDA_FUNCTION_NAME").unwrap_or_else(|_| "function".to_owned());
    format!("arn:aws:lambda:{region}:000000000000:function:{function}")
}

fn bad_request(message: String) -> Response {
    tracing::debug!(message, "Rejecting an invocation request");
    (StatusCode::BAD_REQUEST, message).into_response()
}

fn function_error(status: StatusCode, error: FunctionError) -> Response {
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        error.body,
    )
        .into_response();
    match HeaderValue::from_str(&error.error_type) {
        Ok(value) => {
            response.headers_mut().insert(FUNCTION_ERROR, value);
        }
        Err(_) => {
            response
                .headers_mut()
                .insert(FUNCTION_ERROR, HeaderValue::from_static("Unhandled"));
        }
    }
    response
}

fn invoke_response(outcome: Outcome) -> Response {
    match outcome {
        Outcome::Response(result) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            result,
        )
            .into_response(),
        Outcome::Error(error) => function_error(StatusCode::OK, error),
    }
}

/// A handler's Function URL response, when it returns one rather than a plain value.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FunctionUrlResult {
    status_code: u16,
    #[serde(default)]
    headers: BTreeMap<String, Value>,
    #[serde(default)]
    cookies: Vec<String>,
    body: Option<String>,
    #[serde(default)]
    is_base64_encoded: bool,
}

/// Turns the handler's result into an HTTP response the way Lambda does for a Function URL: an
/// object with a `statusCode` describes the response, and any other result is a `200` with the
/// result as a JSON body.
fn function_url_response(outcome: Outcome) -> Response {
    let result = match outcome {
        Outcome::Response(result) => result,
        Outcome::Error(error) => return function_error(StatusCode::BAD_GATEWAY, error),
    };

    let describes_response = matches!(
        serde_json::from_slice::<Value>(&result),
        Ok(Value::Object(object)) if object.contains_key("statusCode")
    );
    if describes_response.not() {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            result,
        )
            .into_response();
    }

    let described = serde_json::from_slice::<FunctionUrlResult>(&result)
        .map_err(|error| error.to_string())
        .and_then(|described| {
            let status = StatusCode::from_u16(described.status_code)
                .map_err(|_| format!("invalid statusCode {}", described.status_code))?;
            let body = match described.body {
                Some(body) if described.is_base64_encoded => BASE64_STANDARD
                    .decode(body)
                    .map_err(|error| format!("body is not base64: {error}"))?,
                Some(body) => body.into_bytes(),
                None => Vec::new(),
            };
            Ok((status, described.headers, described.cookies, body))
        });

    let (status, headers, cookies, body) = match described {
        Ok(described) => described,
        Err(error) => {
            // Lambda answers a malformed Function URL response with a 502 as well.
            tracing::debug!(
                error,
                "The handler returned an invalid Function URL response"
            );
            let error = FunctionError {
                error_type: "Runtime.InvalidResponse".to_owned(),
                body: json!({
                    "errorType": "Runtime.InvalidResponse",
                    "errorMessage": error,
                })
                .to_string()
                .into(),
            };
            return function_error(StatusCode::BAD_GATEWAY, error);
        }
    };

    let mut response = (status, body).into_response();
    let response_headers = response.headers_mut();
    for (name, value) in headers {
        let value = match value {
            Value::String(value) => value,
            other => other.to_string(),
        };
        match (
            HeaderName::try_from(name.as_str()),
            HeaderValue::try_from(value),
        ) {
            (Ok(name), Ok(value)) => {
                response_headers.insert(name, value);
            }
            _ => tracing::debug!(name, "Dropping an invalid response header"),
        }
    }
    for cookie in cookies {
        if let Ok(cookie) = HeaderValue::try_from(cookie) {
            response_headers.append(header::SET_COOKIE, cookie);
        }
    }

    response
}
