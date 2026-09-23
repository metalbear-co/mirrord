//! Replica id from the ECS task metadata endpoint (v4).

use serde::Deserialize;

use super::{ReplicaIdError, get_json};

/// Set by ECS (EC2 and Fargate) in every container. Points at the container's metadata endpoint,
/// e.g. `http://169.254.170.2/v4/<container-id>`.
const METADATA_URI_ENV: &str = "ECS_CONTAINER_METADATA_URI_V4";

/// The part of the task metadata response that the agent uses.
#[derive(Deserialize)]
struct TaskMetadata {
    #[serde(rename = "TaskARN")]
    task_arn: String,
}

/// The container metadata endpoint of the ECS task the agent runs in.
#[derive(Debug)]
pub(super) struct Endpoint(String);

impl Endpoint {
    pub(super) fn from_env() -> Option<Self> {
        std::env::var(METADATA_URI_ENV).ok().map(Self)
    }

    /// An ECS service scales out by running more tasks, so the task ARN is unique per replica.
    pub(super) async fn task_arn(&self) -> Result<String, ReplicaIdError> {
        let uri = format!("{}/task", self.0.trim_end_matches('/'));
        Ok(get_json::<TaskMetadata>(&uri, &[]).await?.task_arn)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parses_task_arn() {
        let body =
            r#"{"Cluster":"c","TaskARN":"arn:aws:ecs:eu-west-1:123:task/c/abc","Family":"f"}"#;
        let metadata: TaskMetadata = serde_json::from_str(body).unwrap();
        assert_eq!(metadata.task_arn, "arn:aws:ecs:eu-west-1:123:task/c/abc");
    }

    #[tokio::test]
    async fn rejects_non_http_uri() {
        let endpoint = Endpoint("https://169.254.170.2/v4/x".to_owned());
        assert!(matches!(
            endpoint.task_arn().await,
            Err(ReplicaIdError::InvalidUri(_))
        ));
    }
}
