# Preparation

Before starting the walkthrough:

- Follow [`DEMO_START.md`](./DEMO_START.md) to build mirrord, start the frontend, and run the local backend.
- Open the demo UI at <http://localhost:3000/demo>.
- Confirm the local backend responds at <http://localhost:8080/healthz>.
- Reset `sample/capabilities-rust/demo/ecs/mirrord.json` to this baseline configuration:

```json
{
  "target": {
    "path": "serverless/demo-service",
    "namespace": "demo-env"
  },
  "feature": {
    "env": false,
    "network": {
      "outgoing": true,
      "incoming": false
    }
  }
}
```

The backend code snippets in this document should be added to `sample/capabilities-rust/backend/src/main.rs`, relative to the mirrord repository root. Add new routes between the existing `/env` and `/outgoing` routes.

Whenever you edit the backend or `mirrord.json`, stop and rerun the local backend through mirrord. This is a useful customer-facing moment: “Now, to have my change applied, all I have to do is rerun the local backend through mirrord.”

Keep the browser developer tools open with F12 so requests and response headers are easy to inspect.

# Self Introduction

Hi everyone, I’m [your name], and I’m part of the team at MetalBear working on mirrord.

Before we begin, quick show of hands: has everyone here seen a mirrord demo before?

If not, no worries—I’ll give you the quick version.

# What is mirrord?

mirrord lets you run your code locally while giving it the context of a remote workload. You keep the fast local development experience while accessing the network, environment, and identity available to the application in that remote environment.


Today, I’m going to show how that experience works with an AWS ECS workload running on Fargate.

Please feel free to interrupt and ask questions along the way—raise your hand or just start talking.

Before we start, I’ll show three practical use cases:

1. **Outgoing network access:** access resources that are only reachable from the remote workload from my local machine.
2. **Incoming request stealing:** route selected staging requests to my local process without affecting other users.
3. **Remote environment and identity:** use the workload’s environment variables and AWS credentials to test authenticated requests with its real permissions.

In each case, the code stays local, while mirrord provides the context of the remote ECS workload.

# Simple app

I have a simple frontend application that lets me query several endpoints exposed by our backend service.

I can run it against my local backend or against the staging service running as an AWS Fargate task.

First, let’s examine the metadata, environment, and outgoing HTTP request results from the staging service.

# First use case

I’ve just been asked to add an endpoint that prints the ARN of the running ECS task.

Let’s run the application locally and see whether I can retrieve that information.

Run the backend without mirrord first. The request should fail because the ECS metadata endpoint is not available from a normal local process.

Now let’s try to access the AWS metadata endpoint.

As expected, it does not work from my local machine.
The endpoint uses a 169.254 link-local address, so it is only accessible from inside the workload environment.

This brings us to the first use case I want to show:

Accessing remote-only resources from a local process using mirrord’s outgoing network feature.

In this case, the remote-only resource is the AWS ECS metadata endpoint.

Now run the backend through mirrord and repeat the request. The request should succeed.

I asked my favorite coding agent to produce something useful, and it came back with this:

```rust
        .route(
            "/task_id",
            get(
                |axum::extract::State(state): axum::extract::State<AppState>| async move {
                    let body = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_millis(state.outgoing_timeout_ms))
                        .build()
                        .expect("client should build")
                        .get("http://169.254.170.2/v2/metadata")
                        .send()
                        .await
                        .and_then(reqwest::Response::error_for_status)
                        .map_err(|error| (StatusCode::BAD_GATEWAY, error.to_string()))?
                        .text()
                        .await
                        .map_err(|error| (StatusCode::BAD_GATEWAY, error.to_string()))?;
                    body.split_once("\"TaskARN\"")
                        .and_then(|(_, value)| value.split('"').nth(1))
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            (
                                StatusCode::BAD_GATEWAY,
                                "TaskARN missing from metadata".to_owned(),
                            )
                        })
                },
            ),
        )
```
It looks like real agent slop, but it seems to achieve our goal.

I’ll add this route to `sample/capabilities-rust/backend/src/main.rs`, stop the running backend, and rerun it through mirrord. Then I’ll open the advanced settings, select `/task_id`, and send the request to the local backend.

Now we can see that the outgoing feature allows the HTTP request to run through the remote workload environment.

My local application can reach the ECS metadata endpoint, and I can validate the change without deploying it.

What would I need to do on the remote side to make this possible?

Only a few additions are required:

Copy the mirrord_remote bootstrap library into the image.
Set the environment variables, including the API key, which is attached at runtime as a secret.
Identify the running service so mirrord can target it.
Use LD_PRELOAD so mirrord_remote starts when the container boots.

(pause for questions)

# Second Use Case

Now my product managers ask to see the change on their own computers.

I can push the change to staging, but that will affect everyone using the environment.

Or I can shorten the feedback cycle using incoming request stealing and the mirrord Chrome extension.

I can ask them to install the mirrord Chrome extension and configure it to attach a specific header to their requests.

mirrord will then steal only the requests that contain that header and route them to my local process.

```
      "incoming": {
        "mode": "steal",
        "http_filter": { "header_filter": "^baggage: .*mirrord-session=demo-serverless.*$" }
      }
```

I’ll add this `incoming` configuration to `sample/capabilities-rust/demo/ecs/mirrord.json` and rerun the backend through mirrord. Allow a few seconds for the connection to become ready.

Now my product managers can test my local change through the staging frontend, while everyone else continues using the existing staging service.

(quick pause)
They now tell me they actually meant the container ARN, not the task ARN.

All right, let’s ask the agent to update the route:
```rust
        .route(
            "/task_id",
            get(
                |axum::extract::State(state): axum::extract::State<AppState>| async move {
                    let url = env::var("ECS_CONTAINER_METADATA_URI_V4")
                        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
                    let metadata = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_millis(state.outgoing_timeout_ms))
                        .build()
                        .expect("client should build")
                        .get(url)
                        .send()
                        .await
                        .and_then(reqwest::Response::error_for_status)
                        .map_err(|error| (StatusCode::BAD_GATEWAY, error.to_string()))?
                        .json::<serde_json::Value>()
                        .await
                        .map_err(|error| (StatusCode::BAD_GATEWAY, error.to_string()))?;
                    metadata["ContainerARN"]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            (
                                StatusCode::BAD_GATEWAY,
                                format!("ContainerARN missing from metadata, {metadata}")
                                    .to_owned(),
                            )
                        })
                },
            ),
        )
```
Now we get an environment-variable-not-found error.

It looks like we forgot to expose the remote workload’s environment variables.

I’ll set `"env": true` in `sample/capabilities-rust/demo/ecs/mirrord.json` and rerun the backend through mirrord:
```
    "env": true,
```
Now my local process can read the same environment variables that are available to the ECS container. The request is still handled by my local process; mirrord supplies the remote context.

(pause for questions)

# Third Use Case

Without adding another story, I’d like to show one more advantage of running my local process with the remote workload’s context: using the workload’s authentication identity for more accurate testing.

My local process can use the environment variables and credentials attached to the workload and make authenticated outgoing requests with the same role and permissions as the container.

For example, say I now want to add an endpoint that lists S3 buckets.

Normally, I would need to configure AWS credentials on my local machine. Even then, I might be using developer credentials with different permissions from the actual workload.

With mirrord, I can use the temporary credentials AWS provides to the ECS container.

This means the request runs with the exact IAM role and permissions assigned to the workload, rather than with my personal developer credentials.

So, let’s ask the agent to add a `/buckets` endpoint:
```
        .route(
            "/buckets",
            get(|| async move {
                let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
                let buckets: Vec<_> = aws_sdk_s3::Client::new(&config)
                    .list_buckets()
                    .send()
                    .await
                    .map_err(|error| (StatusCode::BAD_GATEWAY, format!("{error:#?}")))?
                    .buckets()
                    .iter()
                    .filter_map(|bucket| bucket.name().map(str::to_owned))
                    .collect();
                Ok::<_, (StatusCode, String)>(Json(buckets))
            }),
        )
```
I’ll add the route to `sample/capabilities-rust/backend/src/main.rs` and rerun the backend through mirrord.

If I run it without mirrord, it fails because my local machine is not configured with AWS credentials. If I send the request to the remote endpoint without stealing, I get a 404 because the new code has not been deployed there.

When I enable stealing, the request is served by my local process. Because outgoing network access and environment access are enabled, the AWS SDK can use the temporary credentials associated with the remote workload to authenticate against AWS APIs.

Without this workflow, I could test the feature locally using broader developer credentials, deploy it, and only then discover that it fails in the real environment.

Getting that feedback immediately saves time, reduces frustration, and helps prevent the classic “it works on my machine” situation.

(pause for questions)

# Outro

So, in just a few minutes, we’ve given a local process access to remote-only resources, routed selected staging traffic to it without affecting anyone else, and tested it with the workload’s real identity and permissions.

And the important part is that we did all of this without building an image, pushing it, deploying it, or waiting for a new task to start.

That is the experience mirrord is aiming for: keep the speed and comfort of local development, while testing in the context where your code will actually run.
