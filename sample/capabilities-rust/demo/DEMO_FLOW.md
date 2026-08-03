# prep

make sure local backend runs before demo
make sure compose is up
make sure mirrord.json is reset
```
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

It is also helpful to keep the browser developer tools open with F12.


Hi everyone, I’m [your name], and I’m part of the team at MetalBear working on mirrord.

Before we begin, quick show of hands: has everyone here seen a mirrord demo before?

If not, no worries—I’ll give you the quick version.

# What is mirrord?

mirrord lets you run your code locally while giving it the context of a remote workload.
That means you keep the fast local development experience, while accessing the network, environment, and identity your application has in the remote environment.


Today, I’m going to show how that experience works with an AWS ECS workload running on Fargate.

Just a quick note: please feel free to interrupt and ask questions along the way, raise a hand or just start talking :)

# Simple app

I have a simple frontend application that lets me query several endpoints exposed by our backend service.

I can run it against my local backend or against the staging service running as an AWS Fargate task.

First, lets examine the respones metadata, environment, and outgoing HTTP request results from the staging service.

# First use case

Now I was just asked to add an endpoint that prints the ARN of the running ECS task.

Let’s run the application locally and see whether I can retrieve that information.

Run - cargo run

Now let's try to access the AWS metadata endpoint.

As expected, it does not work from my local machine.
The endpoint uses a 169.254 link-local address, so it is only accessible from inside the workload environment.

This brings us to the first use case I want to show:

Accessing remote-only resources from a local process using mirrord’s outgoing network feature.

In this case, the remote-only resource is the AWS ECS metadata endpoint.

Run mirrord exec, show that now we can access

I asked my favorite coding agent to produce something useful, he came back with this:

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

I’ll restart the backend, open the advanced settings, set the endpoint to /task, and run the request against the local backend.

Now we can see that the outgoing feature allows the HTTP request to run through the remote workload environment.

My local application can reach the ECS metadata endpoint, and I can validate the change without deploying it.

What do I need to do on the remote side to make this possible?

Only a few additions:

Copy the mirrord_remote bootstrap library into the image.
Set the environment variables, including the API key, which is attached at runtime as a secret.
Identify the running service so mirrord can target it.
Use LD_PRELOAD so mirrord_remote starts when the container boots.

(pause for questions)

# Second Use Case

Now my product managers asks to see the change on their own computer.

I can push the change to staging, but that will affect everyone using the environment.

Or I can expedite the cycle using incoming request stealing and the mirrord Chrome extension.

I can ask them to use the  extension and configure it to attach a specific header to their requests.

mirrord will then steal only the requests that contain that header and route them to my local process.

```
      "incoming": {
        "mode": "steal",
        "http_filter": { "header_filter": "^baggage: .*mirrord-session=demo-serverless.*$" }
      }
```

lets edit mirrord.json and rerun mirrord
(speaking buffer as it takes a few seconds for the connection to kick in)
This means my product managers can test my local change through the staging frontend,
while everyone else continues using the existing staging service.

(quick pause)
Oh, Now they tell me they actually meant the container ARN, not the task ARN.

All right, let’s give the agent another run at slopping
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
Now we get an environment variable not found error.

It looks like we forgot to expose the remote workload’s environment variables.

I’ll enable environment variable access in mirrord.json:
```
    "env": true,
```
Now my local process can read the same environment variables that are available to the ECS container.

(pause for questions)

# Third Use Case

Without adding another story, I’d like to show one more advantage of having my local process run with the remote workload’s context.

Using the same authentication identity as the remote workload for more accurate testing.

My local process can use the environment variables and credentials attached to the workload and make authenticated outgoing requests with the same role and permissions as the container.

For example, say I now want to add an endpoint that lists S3 buckets.

Normally, I would need to configure AWS credentials on my local machine. Even then, I might be using developer credentials with different permissions from the actual workload.

With mirrord, I can use the temporary credentials AWS provides to the ECS container.

This means the request runs with the exact IAM role and permissions assigned to the workload, rather than with my personal developer credentials.

So, let’s slop the slop:
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
Let’s run it.

Okay, I’m missing the s3:ListAllMyBuckets permission.

I’ll stop here, because this result already demonstrates the value.

I get immediate feedback using the workload’s real identity and actual permissions.

Without this workflow, I could test the feature locally using broader developer credentials, deploy it, and only then discover that it fails in the real environment.

Getting that feedback immediately saves time, reduces frustration, and helps prevent the classic “it works on my machine” situation.

(pause for questions)

# Outro

So, in just a few minutes, we’ve taken a local process and given it access to remote-only resources, routed selected staging traffic to it without affecting anyone else, and tested it using the workload’s real identity and permissions.

And the important part is that we did all of this without building an image, pushing it, deploying it, or waiting for a new task to start.

That is the experience mirrord is aiming for: keep the speed and comfort of local development, while testing in the context where your code will actually run.
