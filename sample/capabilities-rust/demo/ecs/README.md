# capabilities-rust ECS setup

This folder contains the ECS-specific helper script and sample assets for the `capabilities-rust` demo.

The Terraform-managed AWS environment in `infra/environments/s8s-aws` is the
source of truth for what runs on AWS. The service published there is the one in
`metalbear-co/playground` under `apps/aws-playground/demo-service`, not the
backend in this folder.

The backend here is a local-development sample. It dumps its whole environment
over `/env` and fetches arbitrary URLs on request through `/outgoing`, which
together hand any caller the credentials of whatever it runs as. Run it locally;
do not deploy it anywhere reachable from the internet.

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

## Applying the Terraform root

```bash
cd ../../../../infra/environments/s8s-aws
terraform init
terraform apply
```

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

- `https://aws-playground.metalbear.dev` — demo service
- `https://session-manager.aws-playground.metalbear.dev/sm` — sessions-manager,
  which requires the `x-mirrord-sm-auth` header. Set
  `MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN` to the value in Secrets Manager under
  `s8s/sessions-manager-auth-header` before connecting.

## Notes

- The old hand-managed ECS task-definition JSON flow is no longer the source of truth.
- The helper script lives next to the ECS sample so the build/deploy flow stays close to the demo assets.
