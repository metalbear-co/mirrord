---
title: "ECS Sample App"
description: "Deploy a sample service to Amazon ECS with Terraform, then run it on your laptop with mirrord through MetalBear Cloud: steal requests by header and reach a private RDS database."
tags:
  - alpha
  - team
  - enterprise
---

This tutorial deploys a small orders API to Amazon ECS on Fargate, next to a private Postgres database on Amazon RDS. Then you run the same service on your laptop with mirrord, and see two things:

* **Real requests reach your laptop:** requests to the public load balancer that carry your session header reach the process on your laptop. All other requests still go to the ECS task.
* **Your laptop reads the real database:** your local process reads and writes the orders in the RDS database, which has no public endpoint. mirrord sends the database connection through the ECS task, inside the VPC. It also gives the local process the task's environment variables, so the process gets the database settings from the task.

The ECS task and your laptop connect to each other through MetalBear Cloud. The code and the Terraform configuration are in the [mirrord-ecs-sample](https://github.com/metalbear-co/mirrord-ecs-sample) repository.

## How it works

![The client sends requests to the load balancer and the ECS task. The mirrord agent in the task sends requests with your header through the MetalBear sessions-manager to the app on your machine. The task connects to the private RDS database for both apps.](../../.gitbook/assets/ecs-sample-app.svg)

The ECS task and your laptop both make only outbound HTTPS connections to the MetalBear sessions-manager, which pairs them. mirrord needs no inbound access to your VPC. The public load balancer is part of the sample app, so that you can send requests to it.

## Prerequisites

1. An AWS account, and the AWS CLI signed in to it.
2. Terraform 1.6 or later.
3. Python 3.10 or later.
4. The mirrord CLI, version 3.288.0 or later.
5. A MetalBear account. To generate the API key, you must be an admin of your MetalBear organization.

{% hint style="info" %}
This tutorial creates billable AWS resources: a NAT gateway, an Application Load Balancer, a `db.t4g.micro` RDS instance and one Fargate task. Run `terraform destroy` when you finish; see [Clean up](#clean-up).
{% endhint %}

---

## Get an API key

The ECS task signs in to MetalBear Cloud with the serverless agent API key of your organization. Your organization has one active key at a time. If it already has a key, ask the admin who generated it for the key, and go to the next step.

1. Open the [MetalBear dashboard](https://app.metalbear.com), and go to **Settings → Serverless agents**.
2. Generate the key.
3. Copy the key, and keep it. It begins with `metalbear_key_`. The dashboard shows the key only once.

This guide calls the key `<API_KEY>`.

## Deploy the service to ECS

Clone the repository and put your API key in the Terraform variables file:

```bash
git clone https://github.com/metalbear-co/mirrord-ecs-sample.git
cd mirrord-ecs-sample/terraform
cp terraform.tfvars.example terraform.tfvars
```

In `terraform.tfvars`, set:

```hcl
mirrord_api_key = "<API_KEY>"
```

The resources go to `us-east-1`. To use a different region, also set `region`.

Deploy:

```bash
terraform init
terraform apply
```

The apply takes several minutes, mostly for the RDS database. Terraform stores the API key in AWS Secrets Manager, and ECS gives it to the task as `MIRRORD_SESSIONS_MANAGER_API_KEY`.

When the apply finishes, keep the service URL, and send a request to it:

```bash
export URL=$(terraform output -raw url)
curl $URL/orders
```

The ECS task sends the response, with `"version": "v1"` and three orders from the database. The task needs 1 or 2 minutes to start after the apply. If the load balancer returns `503`, wait, and send the request again.

## Run the service locally

Sign in to your MetalBear account once:

```bash
mirrord login
```

The sign-in must be to the organization that owns the API key. For more, see [Developer setup](metalbear-cloud.md#developer-setup).

Install the app's dependencies:

```bash
cd ../app
python3 -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt
```

Open `main.py` and change the code. For example:

```python
GREETING = "Hello from my laptop"
VERSION = "v2"
```

Run the app with mirrord, from the root of the repository:

```bash
cd ..
mirrord up --key $USER
```

`mirrord up` reads `mirrord-up.yaml` and starts the app with `python app/main.py`. For the full configuration, see [The mirrord configuration](#the-mirrord-configuration).

When the app starts, it creates the `orders` table in RDS if the table does not exist. So the log line `Application startup complete` also shows that your local process reached the private database.

## Send requests

`mirrord up` keeps its terminal busy. Open a second terminal, go to the `terraform` directory of the repository, and set the service URL again:

```bash
export URL=$(terraform output -raw url)
```

mirrord sends only some requests to your laptop: the HTTP requests whose `baggage` header contains `mirrord-session=<your username>`. This is your session header. Your username is the session key, which you gave to `--key` as `$USER`. Each developer uses a different session key, so each one gets only their own requests. If several developers have the same username, for example `root` in dev containers, give each one a different `--key`.

![A request with your header goes from the load balancer to the ECS task, and then through MetalBear Cloud to the orders app on your machine, which answers with v2. A request without the header goes to the orders app in the task, which answers with v1.](../../.gitbook/assets/ecs-sample-app-requests.svg)

Send a request with your session header:

```bash
curl -H "baggage: mirrord-session=$USER" $URL/
curl -H "baggage: mirrord-session=$USER" $URL/orders
```

Both responses show `"version": "v2"`, so your local code served them. The `/orders` response also lists the rows from the RDS database, which your laptop has no route to.

Send the same request without the header:

```bash
curl $URL/orders
```

The response shows `"version": "v1"`. The ECS task still serves all other traffic, so other developers are not affected.

Add an order from your local code, and then read it from the ECS task:

```bash
curl -X POST -H "baggage: mirrord-session=$USER" \
  -H "content-type: application/json" -d '{"item": "bagel"}' $URL/orders
curl $URL/orders
```

The ECS task returns the new order, because both processes use the same database.

To try more changes, change the code, stop `mirrord up`, and start it again. You do not build an image or deploy between changes.

---

## The mirrord configuration

The repository's `mirrord-up.yaml` is:

```yaml
common:
  operator: false
services:
  orders:
    target:
      path: serverless/orders
      namespace: staging
    run:
      command: ["python", "app/main.py"]
```

| Setting | Value | What it does |
| --- | --- | --- |
| `common.operator` | `false` | Connects through MetalBear Cloud. Without it, mirrord looks for an Operator in your current kubeconfig context, and uses it if it finds one. |
| `services.orders.target.path` | `serverless/orders` | Selects the service. It matches `MIRRORD_REMOTE_SERVICE` on the task. |
| `services.orders.target.namespace` | `staging` | Selects the environment. It matches `MIRRORD_REMOTE_ENVIRONMENT` on the task. |
| `services.orders.run.command` | `python app/main.py` | The local process that mirrord runs. |

The service uses the default `split` mode of `mirrord up`. In this mode, mirrord steals only the HTTP requests whose `baggage` header contains `mirrord-session=<key>`. All other requests stay with the ECS task. For more, see [mirrord up](../multiple-concurrent-sessions.md).

`--key $USER` sets the session key to your username. To use a different key, change `--key`, or set the `MIRRORD_KEY` environment variable. Without either, `mirrord up` makes a key from your username. Send the same key in the header.

## The task definition

The task definition in [`terraform/ecs.tf`](https://github.com/metalbear-co/mirrord-ecs-sample/blob/main/terraform/ecs.tf) has the changes from [Amazon ECS](ecs.md) and [Connecting ECS to MetalBear Cloud](ecs-metalbear-cloud.md):

1. A setup container copies the [remote bootstrap](ecs.md) into a shared volume before the app starts.
2. The app container loads the bootstrap with `LD_PRELOAD`. The app image does not change.
3. `MIRRORD_REMOTE_SERVICE` and `MIRRORD_REMOTE_ENVIRONMENT` name the workload.
4. ECS injects the API key from Secrets Manager as `MIRRORD_SESSIONS_MANAGER_API_KEY`.

The service runs one task, because one replica serves a mirrord session. The app image is based on `python:3.12-slim-bookworm`, because the bootstrap needs glibc 2.30 or later. For all the requirements, see [Application container requirements](README.md#application-container-requirements).

To use the same setup with the mirrord Operator on EKS, see [Operator-hosted mode](https://github.com/metalbear-co/mirrord-ecs-sample#operator-hosted-mode) in the repository.

## Clean up

From the `terraform` directory of the repository, delete all the resources:

```bash
terraform destroy
```

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| `mirrord operator does not serve the operator-hosted sessions-manager` | `mirrord-up.yaml` has no `operator: false` under `common`, and your current kubeconfig context has an Operator. Add `operator: false` under `common`. |
| `Feature target.path requires a mirrord Enterprise plan` | `mirrord-up.yaml` has no `operator: false` under `common`, and your current kubeconfig context has an Operator with a Free or Team license. Add `operator: false` under `common`. |
| `mirrord up` cannot find the target | `target.path` or `target.namespace` in `mirrord-up.yaml` does not match the task's `MIRRORD_REMOTE_SERVICE` or `MIRRORD_REMOTE_ENVIRONMENT`, or the task is not running. |
| A request with the header gets `"version": "v1"` | The header does not match the filter. Send `baggage: mirrord-session=<key>`, with the key that you gave to `--key`. |
| `mirrord up` asks you to sign in, or refuses the session | Your sign-in expired, or it is to a different organization from the API key's. Run `mirrord login` again. |
| `` `orders` is `split` mode with a serverless target `` | Your mirrord CLI is earlier than 3.288.0. Update the CLI. |

For other problems, see [Connecting ECS to MetalBear Cloud](ecs-metalbear-cloud.md#troubleshooting) and [Amazon ECS](ecs.md#troubleshooting).
