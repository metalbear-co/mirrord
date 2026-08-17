use eventsource_stream::Event;
use serde::Serialize;
use url::Url;

use super::event::ControlPlaneEvent;
use crate::error::SessionsManagerClientError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ApiVersion {
    #[default]
    V1,
}

impl ApiVersion {
    const fn path_segment(self) -> &'static str {
        match self {
            Self::V1 => "v1",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ControlPlaneApi {
    base_url: Url,
    version: ApiVersion,
}

pub(crate) enum ControlPlaneEndpoint<'a> {
    Assignments {
        environment: &'a str,
        service: &'a str,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub(crate) enum AssignmentSubscription {
    Agent {
        replica_id: String,
    },
    Intproxy {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        target_replica_id: Option<String>,
    },
}

impl ControlPlaneApi {
    pub(crate) fn new(base_url: Url) -> Self {
        Self {
            base_url,
            version: ApiVersion::V1,
        }
    }

    pub(crate) fn endpoint(
        &self,
        endpoint: ControlPlaneEndpoint<'_>,
    ) -> Result<Url, SessionsManagerClientError> {
        let mut url = self.base_url.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| SessionsManagerClientError::InvalidBaseUrl)?;
            segments.pop_if_empty();
            segments.push(self.version.path_segment());

            match endpoint {
                ControlPlaneEndpoint::Assignments {
                    environment,
                    service,
                } => {
                    segments.extend(["env", environment, "service", service, "assignments"]);
                }
            }
        }
        Ok(url)
    }

    pub(crate) fn decode_event(
        &self,
        event: Event,
    ) -> Result<Option<ControlPlaneEvent>, SessionsManagerClientError> {
        match self.version {
            ApiVersion::V1 => self.decode_v1_event(event),
        }
    }

    fn decode_v1_event(
        &self,
        event: Event,
    ) -> Result<Option<ControlPlaneEvent>, SessionsManagerClientError> {
        match event.event.as_str() {
            "assignment" => Ok(Some(ControlPlaneEvent::Assignment(serde_json::from_str(
                &event.data,
            )?))),
            "message" if event.data.is_empty() => Ok(None),
            event_name => {
                tracing::trace!(event_name, "ignoring unknown sessions-manager SSE event");
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{AssignmentSubscription, ControlPlaneApi, ControlPlaneEndpoint};
    use crate::control_plane::ControlPlaneEvent;

    fn assignments_endpoint(base_url: &str, environment: &str, service: &str) -> url::Url {
        ControlPlaneApi::new(url::Url::parse(base_url).unwrap())
            .endpoint(ControlPlaneEndpoint::Assignments {
                environment,
                service,
            })
            .unwrap()
    }

    fn query_pairs(subscription: AssignmentSubscription) -> HashMap<String, String> {
        reqwest::Client::new()
            .get("https://example.com")
            .query(&subscription)
            .build()
            .unwrap()
            .url()
            .query_pairs()
            .into_owned()
            .collect()
    }

    fn event(event: &str, data: &str) -> eventsource_stream::Event {
        eventsource_stream::Event {
            event: event.to_owned(),
            data: data.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn assignments_endpoint_includes_v1() {
        assert_eq!(
            assignments_endpoint("https://sessions.example.com", "staging", "api").as_str(),
            "https://sessions.example.com/v1/env/staging/service/api/assignments"
        );
    }

    #[test]
    fn assignments_endpoint_preserves_base_path_prefix() {
        assert_eq!(
            assignments_endpoint("https://sessions.example.com/sm", "staging", "api").as_str(),
            "https://sessions.example.com/sm/v1/env/staging/service/api/assignments"
        );
    }

    #[test]
    fn assignments_endpoint_ignores_base_url_trailing_slash() {
        let without_trailing_slash =
            assignments_endpoint("https://sessions.example.com/sm", "staging", "api");
        let with_trailing_slash =
            assignments_endpoint("https://sessions.example.com/sm/", "staging", "api");

        assert_eq!(without_trailing_slash, with_trailing_slash);
    }

    #[test]
    fn assignments_endpoint_escapes_dynamic_segments() {
        assert_eq!(
            assignments_endpoint("https://sessions.example.com", "prod/eu", "api #1").as_str(),
            "https://sessions.example.com/v1/env/prod%2Feu/service/api%20%231/assignments"
        );
    }

    #[test]
    fn serializes_agent_subscription() {
        assert_eq!(
            query_pairs(AssignmentSubscription::Agent {
                replica_id: "pod-a".to_owned(),
            }),
            HashMap::from([
                ("role".to_owned(), "agent".to_owned()),
                ("replica_id".to_owned(), "pod-a".to_owned()),
            ])
        );
    }

    #[test]
    fn serializes_intproxy_subscription() {
        assert_eq!(
            query_pairs(AssignmentSubscription::Intproxy {
                session_id: "session-a".to_owned(),
                target_replica_id: Some("pod-a".to_owned()),
            }),
            HashMap::from([
                ("role".to_owned(), "intproxy".to_owned()),
                ("session_id".to_owned(), "session-a".to_owned()),
                ("target_replica_id".to_owned(), "pod-a".to_owned()),
            ])
        );
    }

    #[test]
    fn omits_missing_intproxy_target_replica_id() {
        assert_eq!(
            query_pairs(AssignmentSubscription::Intproxy {
                session_id: "session-a".to_owned(),
                target_replica_id: None,
            }),
            HashMap::from([
                ("role".to_owned(), "intproxy".to_owned()),
                ("session_id".to_owned(), "session-a".to_owned()),
            ])
        );
    }

    #[test]
    fn decodes_assignment_event() {
        let api = ControlPlaneApi::new(url::Url::parse("https://sessions.example.com").unwrap());
        let event = event(
            "assignment",
            r#"{"data_plane_endpoint":"/sm/ws/123","authorization":"Bearer secret"}"#,
        );

        assert!(matches!(
            api.decode_event(event).unwrap(),
            Some(ControlPlaneEvent::Assignment(_))
        ));
    }

    #[test]
    fn ignores_unknown_event() {
        let api = ControlPlaneApi::new(url::Url::parse("https://sessions.example.com").unwrap());

        assert!(api.decode_event(event("other", "{}")).unwrap().is_none());
    }

    #[test]
    fn rejects_malformed_assignment_event() {
        let api = ControlPlaneApi::new(url::Url::parse("https://sessions.example.com").unwrap());

        assert!(api.decode_event(event("assignment", "not json")).is_err());
    }
}
