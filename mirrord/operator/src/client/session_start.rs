//! The operator's record of what it did while a session was starting, as
//! `mirrord diagnose timings` reads it.
//!
//! The operator keeps these in memory and serves them at `sessions/timings`. They are read
//! leniently, because operators newer than this client add fields and phase names: unknown
//! fields are dropped, and a phase is a name, the operator's description of it and a duration,
//! whatever the name is.

use kube::Client;
use serde::{Deserialize, Serialize};

use crate::client::{
    ClientCertificateState, OperatorApi,
    error::{OperatorApiError, OperatorApiResult, OperatorOperation},
};

/// One step of a connect, as the operator named and timed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPhase {
    pub name: String,
    /// What the phase covers, in the operator's words. The operator defines the phases, so it
    /// is the one that describes them. Empty from an operator that sends none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub duration_ms: u64,
    /// Set on the phase a connect ended in without finishing it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

/// One connection the session made to the operator while starting. A session makes several (the
/// CLI's own, then the internal proxy's), and the first one carries the slow phases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartConnect {
    pub started_at: String,
    /// In the order the connect went through them. A phase the connect did not need is absent,
    /// and a connect that is still running lists only the phases it has finished.
    #[serde(default)]
    pub phases: Vec<StartPhase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionStart {
    session_id: String,
    #[serde(default)]
    connects: Vec<StartConnect>,
}

#[derive(Deserialize)]
struct SessionStartList {
    #[serde(default)]
    sessions: Vec<SessionStart>,
}

/// What the operator answered when asked about one session's start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStartTimings {
    /// The session's connects, oldest first.
    Recorded(Vec<StartConnect>),
    /// The operator records timings and has none for this session: it never saw the session,
    /// dropped it to make room for newer ones, or restarted since.
    Forgotten,
    /// The operator predates the route.
    Unsupported,
}

impl<C> OperatorApi<C>
where
    C: ClientCertificateState,
{
    /// Asks the operator what it did while the session with `session_id` (its id in hex, as
    /// session views print it) was starting.
    ///
    /// Tries the cluster-wide route first. A user whose role only covers some namespaces is
    /// refused there, so the route under `namespace` is tried next when one is given.
    pub async fn session_start_timings(
        &self,
        session_id: &str,
        namespace: Option<&str>,
    ) -> OperatorApiResult<SessionStartTimings> {
        let cluster_wide =
            format!("/apis/operator.metalbear.co/v1/sessions/timings?sessionId={session_id}");
        let answer = match (
            request_timings(self.client(), cluster_wide).await,
            namespace,
        ) {
            (Err(kube::Error::Api(status)), Some(namespace)) if status.code == 403 => {
                let namespaced = format!(
                    "/apis/operator.metalbear.co/v1/namespaces/{namespace}/sessions/timings?sessionId={session_id}"
                );
                request_timings(self.client(), namespaced).await
            }
            (answer, _) => answer,
        };

        match answer {
            Ok(list) => Ok(list
                .sessions
                .into_iter()
                .find(|session| session.session_id.eq_ignore_ascii_case(session_id))
                .map_or(SessionStartTimings::Forgotten, |session| {
                    SessionStartTimings::Recorded(session.connects)
                })),
            // To an operator without the route this is a `get` of a session named `timings`.
            Err(kube::Error::Api(status)) if status.code == 404 => {
                Ok(SessionStartTimings::Unsupported)
            }
            Err(error) => Err(OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::ReadingSessionStartTimings,
            }),
        }
    }
}

async fn request_timings(client: &Client, uri: String) -> kube::Result<SessionStartList> {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .body(Vec::new())
        .map_err(kube::Error::HttpError)?;

    client.request(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer of a real operator, plus a field and a phase name this client does not know.
    /// Operators add both over time, and a CLI that rejected them would lose the whole section.
    #[test]
    fn an_answer_with_unknown_fields_and_phases_is_read() {
        let answer = r#"{"sessions":[{"sessionId":"5E1D5B32275A0332","region":"eu","connects":[
            {"startedAt":"2026-11-25T01:11:54.660513Z","attempt":1,"phases":[
                {"name":"target_lookup","durationMs":1293},
                {"name":"warp_drive_spinup","durationMs":7},
                {"name":"queue_splits","durationMs":72824,"failed":true}]},
            {"startedAt":"2026-11-25T01:13:15.161211Z"}]}]}"#;

        let list: SessionStartList = serde_json::from_str(answer).unwrap();

        let [session] = list.sessions.as_slice() else {
            panic!("expected one session");
        };
        assert_eq!(session.session_id, "5E1D5B32275A0332");
        let phases = session
            .connects
            .iter()
            .map(|connect| {
                connect
                    .phases
                    .iter()
                    .map(|phase| (phase.name.as_str(), phase.duration_ms, phase.failed))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            phases,
            [
                vec![
                    ("target_lookup", 1293, false),
                    ("warp_drive_spinup", 7, false),
                    ("queue_splits", 72824, true),
                ],
                vec![],
            ]
        );
    }
}
