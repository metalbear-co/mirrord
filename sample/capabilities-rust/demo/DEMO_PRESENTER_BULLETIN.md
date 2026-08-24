# Demo Presenter Notes

## Introduction

* Introduce myself:

  * Name
  * MetalBear
  * Working on mirrord
* Ask: **“Has everyone seen a mirrord demo before?”**
* For anyone new:

  * **Run code locally**
  * **Give it the context of a remote workload**
  * Keep local development speed
  * Access the remote workload’s network, environment, and identity
* Today’s environment:

  * AWS ECS
  * Fargate workload
* Invite questions:

  * **“Please feel free to interrupt, raise a hand, or just start talking.”**

---

# What is mirrord?

### Core message

**Local process, remote context.**

* Develop locally
* Interact with real remote resources
* Test against the environment where the code will actually run
* Avoid repeated build, push, and deploy cycles

### Transition

**“Let’s see what that looks like in practice.”**

---

# Simple App

### Show the application

* Simple frontend
* Queries several backend endpoints
* Can point it at:

  * Local backend
  * Staging backend running on Fargate

### Start with staging

*Show the staging responses.*

* Metadata
* Environment
* Outgoing HTTP request

### Transition

**“Now let’s make a change locally.”**

---

# Use Case 1: Accessing Remote-Only Resources

### Set up the story

* I’ve just been asked to add an endpoint
* It should return the running ECS task ARN

### Run locally

*Run:*

`cargo run`

*Try the AWS metadata request.*

### Expected failure

* Request fails locally
* Metadata endpoint uses a `169.254` link-local address
* Only accessible from inside the workload environment

### Key message

**mirrord outgoing networking lets a local process access remote-only resources.**

* Local code
* Outgoing request happens in the workload’s context
* Resource in this example: AWS ECS metadata endpoint

### Enable mirrord

*Run the backend with `mirrord exec`.*

*Show that the metadata endpoint is now accessible.*

### Introduce the generated code

**“I asked my favorite coding agent to produce something useful, and it came back with this.”**

*Show the first `/task_id` implementation.*

### Light joke

**“It looks like real agent slop, but it seems to achieve our goal.”**

### Test the implementation

* Restart the backend
* Open advanced settings
* Set the endpoint to `/task`
* Send the request to the local backend

### Explain the result

* Local application reaches the ECS metadata endpoint
* Outgoing request uses the remote workload environment
* Change is validated without deploying

### Remote-side setup

**“What did we need to change on the remote side?”**

* Copy the `mirrord_remote` bootstrap library into the image
* Provide environment variables
* Attach the API key at runtime as a secret
* Identify the service so mirrord can target it
* Use `LD_PRELOAD` to start `mirrord_remote` when the container boots

*Pause for questions.*

---

# Use Case 2: Sharing a Local Change

### Set up the story

* Product manager wants to see the change from their computer
* One option: deploy it to staging
* Problem: that affects everyone using staging

### Introduce the alternative

**Incoming request stealing with the mirrord Chrome extension.**

* Product manager uses the extension
* Extension adds a specific header
* mirrord steals only requests containing that header
* Matching requests go to my local process
* Everyone else continues using the normal staging service

### Update the configuration

*Show the `incoming` configuration.*

*Edit `mirrord.json`.*

*Restart mirrord.*

### Speaking buffer while mirrord connects

**“What’s happening here is that mirrord is establishing the connection between my local process and the remote service.”**

**“Once that is ready, only the requests carrying our session header will be routed to me.”**

**“The staging environment itself stays available and unchanged for everyone else.”**

### Demonstrate

* Send a normal request

  * Goes to staging
* Send a request with the extension header

  * Goes to the local backend

### Key message

**One shared environment, isolated personal development sessions.**

---

# Requirement Change

*Quick pause.*

### Continue the live story

**“And now they tell me they actually meant the container ARN, not the task ARN.”**

**“All right, let’s give the agent another run at slopping.”**

*Show the updated implementation.*

### Run it

* Receive: `environment variable not found`

### Explain the issue

* New implementation reads `ECS_CONTAINER_METADATA_URI_V4`
* That variable exists in the ECS container
* It is not currently exposed to the local process

### Enable environment access

*Set:*

`"env": true`

*Restart and run again.*

### Result

* Local process receives the workload’s environment variables
* Code can read the ECS metadata URI
* Container ARN can now be retrieved

### Key message

**The local process can use the same environment as the remote workload.**

*Pause for questions.*

---

# Use Case 3: Using the Workload’s Identity

### Introduce the capability

**“There’s one more advantage of running my local process with the remote workload’s context.”**

**Use the same authentication identity as the workload for more accurate testing.**

### Explain why this matters

* Local process can use credentials attached to the ECS workload
* Authenticated outgoing requests use the workload’s role
* No need to configure personal AWS credentials
* Tests use the workload’s real permissions
* More accurate than testing with broad developer permissions

### Example

* Add an endpoint that lists S3 buckets
* AWS gives temporary credentials to the ECS container
* mirrord exposes the relevant environment and network context locally

### Introduce the code

**“So, let’s slop the slop.”**

*Show the `/buckets` implementation.*

### Run it

* Request fails
* Missing permission: `s3:ListAllMyBuckets`

### Do not frame this as a failed demo

**“This is actually the feedback I wanted.”**

* Request used the real workload identity
* It exposed the workload’s actual permissions
* Failure was discovered immediately
* No deployment required

### Contrast with the traditional workflow

Without mirrord:

* Test locally with broader developer credentials
* Everything appears to work
* Build and push an image
* Deploy it
* Wait for the task to start
* Discover the real workload lacks permission

With mirrord:

* Receive the same feedback immediately
* Stay inside the local development loop

### Key message

**More accurate testing, faster feedback, and fewer ‘works on my machine’ surprises.**

*Pause for questions.*

---

# Outro

### Recap the three capabilities

In a few minutes, we:

1. Gave a local process access to a remote-only resource
2. Routed selected staging traffic to it without affecting other users
3. Tested with the workload’s real identity and permissions

### Emphasize what we avoided

* No image build
* No image push
* No deployment
* No waiting for a new task
* No disruption to the shared staging environment

### Final message

**mirrord keeps the speed and comfort of local development while letting you test in the context where your code will actually run.**

**The goal isn’t just to make remote development faster. It’s to make local feedback more realistic.**

**Because the best way to avoid “it works on my machine” is to make your machine work like the real environment.**

**Thank you. I’m happy to take questions.**
