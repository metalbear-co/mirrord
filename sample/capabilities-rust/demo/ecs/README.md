# capabilities-rust ECS setup

This folder contains the ECS-specific helper script and sample assets for the `capabilities-rust` demo.

The Terraform-managed AWS environment in `infra/environments/s8s-aws` is the source of truth now:

- `core/` manages the shared AWS infrastructure and `sessions-manager`
- `demo-addon/` manages the optional backend/frontend demo application layer

## Prerequisites

This helper expects the `mirrord`, `operator`, and `infra` repositories to be checked out side-by-side under the same parent directory.

Before you build or deploy anything, make sure you are authenticated to AWS and Docker can push to ECR:

1. Log in to AWS using whichever flow your account uses (`aws sso login`, short-lived credentials, etc.). A quick sanity check is:

```bash
aws sts get-caller-identity
```

2. Log Docker in to the ECR registry used by the demo:

```bash
aws ecr get-login-password --region us-east-1 \
  | docker login --username AWS --password-stdin 013388577054.dkr.ecr.us-east-1.amazonaws.com
```

## Applying the Terraform roots

If this is a fresh checkout, apply the shared core root first, then the addon root:

```bash
cd ../../../../infra/environments/s8s-aws/core
terraform init
terraform apply

cd ../demo-addon
terraform init
terraform apply
```

The core root exports the shared ALB, ECS cluster, IAM roles, subnets, and `sessions-manager` URL that the addon consumes via `terraform_remote_state`.

## Building, pushing, and deploying the images

Build the image locally from this directory with `build.sh`:

```bash
./build.sh <backend|frontend-next|sessions-manager> [local-tag] [--debug]
```

Examples:

```bash
./build.sh sessions-manager
./build.sh backend
./build.sh frontend-next
./build.sh backend --debug
```

What the script does:

- builds the requested image locally
- tags it with the local image tag

Useful flags:

- `--debug` builds a debug image instead of release

The AWS-side push and deployment flow lives in `../../../../infra/environments/s8s-aws/push_and_deploy.sh`.

## Demo endpoints

Once both roots are applied and the images are deployed, the public endpoints are:

- `http://sm.s8s.staging.metalbear.com`
- `http://demo.s8s.staging.metalbear.com`

The demo backend is available at:

- `http://demo.s8s.staging.metalbear.com/demo/api`

## Notes

- The old hand-managed ECS task-definition JSON flow is no longer the source of truth.
- The helper script lives next to the ECS sample so the build/deploy flow stays close to the demo assets.
