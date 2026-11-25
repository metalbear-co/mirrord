//! The AWS Batch job of the task the workload companion runs in.
//!
//! The companion only reports which job it runs in. The operator looks up the job's tags with its
//! own AWS role, so the companion never calls AWS, and a task can't claim tags it doesn't have.

use mirrord_sessions_manager_client::AwsBatchJob;

/// Set by AWS Batch in every container of a job.
const JOB_ID_ENV: &str = "AWS_BATCH_JOB_ID";

/// The region variables the AWS SDKs read, in the order they read them.
const REGION_ENVS: [&str; 2] = ["AWS_REGION", "AWS_DEFAULT_REGION"];

/// The Batch job of this task, or [`None`] when it doesn't run in one, or its region is unknown.
///
/// The region comes from [`REGION_ENVS`], and then from `replica_id` when it is an ECS task ARN,
/// which is what [`resolve_replica_id`](super::resolve_replica_id) reports on ECS.
pub(crate) fn aws_batch_job(replica_id: &str) -> Option<AwsBatchJob> {
    let job_id = non_empty_env(JOB_ID_ENV)?;
    let region = REGION_ENVS
        .into_iter()
        .find_map(non_empty_env)
        .or_else(|| ecs_task_region(replica_id).map(str::to_owned));
    let Some(region) = region else {
        tracing::debug!(
            "Running in an AWS Batch job of an unknown region, so the job's tags can't be looked up"
        );
        return None;
    };

    Some(AwsBatchJob { job_id, region })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The region of an ECS task ARN, `arn:<partition>:ecs:<region>:<account>:task/<cluster>/<id>`.
fn ecs_task_region(arn: &str) -> Option<&str> {
    let parts = arn.splitn(6, ':').collect::<Vec<_>>();
    match parts.as_slice() {
        ["arn", _, "ecs", region, _, resource]
            if !region.is_empty() && resource.starts_with("task/") =>
        {
            Some(region)
        }
        _ => None,
    }
}
