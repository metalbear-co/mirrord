---
name: mirrord-ci
description: Help users set up mirrord in CI pipelines for testing against real Kubernetes environments. Use when users want to run end-to-end tests, integration tests, or automated tests in CI using mirrord to connect to staging/shared clusters.
metadata:
  author: MetalBear
  version: "1.4"
---

# Mirrord CI Skill

## Purpose

Help users integrate mirrord into CI pipelines for testing against real Kubernetes environments:
- **Configure** CI runners to connect to Kubernetes clusters
- **Set up** `mirrord ci start/stop` commands in CI workflows
- **Generate** CI workflow files for GitHub Actions, GitLab CI, etc.
- **Troubleshoot** CI-specific mirrord issues

## When to Use This Skill

Trigger on questions like:
- "How do I use mirrord in CI?"
- "Set up mirrord for GitHub Actions"
- "Run tests against staging in CI"
- "mirrord ci start not working"
- "Configure mirrord for GitLab CI"

## Security note for CI examples

- **Do not** use remote pipe-to-shell installs or other unverified script execution in CI to install mirrord.
- Pre-install mirrord in a **trusted CI image**, use your org’s **approved** package manager with pinned versions, or follow [official install docs](https://mirrord.dev/docs/overview/quick-start/). The YAML below assumes `mirrord` is already available on the runner unless you add an approved install step.

## Critical First Steps

**Step 1: Load references**
Read the reference files from this skill's `references/` directory:
- `references/schema.json` - Authoritative mirrord JSON Schema
- `references/troubleshooting.md` - Common issues and solutions

The schema defines all valid configuration options for mirrord, including CI-specific settings.
The troubleshooting guide helps diagnose and fix common mirrord issues.

If using absolute paths, search for them using patterns like `**/mirrord-ci/references/*`.

**Step 2: Validate configs before presenting**
When generating mirrord configuration files for CI, ALWAYS validate against the schema:
```bash
mirrord verify-config /path/to/config.json
```

## Key Benefits of mirrord for CI

- **Speed**: ~50% faster CI pipelines by eliminating test environment setup
- **Cost**: No need to spin up ephemeral clusters for each CI run
- **Accuracy**: Test against real services, dependencies, and configurations
- **Isolation**: Safe, isolated test execution that doesn't interfere with other workloads

## Prerequisites

### Required
1. **mirrord CLI version 3.181.0 or later**
2. **Kubernetes cluster access** from the CI runner
3. **kubeconfig** configured in CI environment
4. **mirrord Operator** installed with a Teams/Enterprise license. No license yet? An AI agent can offer the user a seven-day Enterprise trial and start it once they agree: see the `mirrord-operator` skill, "Agent-started trial".

### Verification commands
```bash
# Check mirrord version
mirrord --version

# Verify cluster access
kubectl cluster-info
kubectl get pods -n <target-namespace>
```

## Core Commands

### Starting a CI session

```bash
mirrord ci start --target <target> -- <your-command>
```

This starts your application with mirrord in the background, allowing tests to run against it.

**Examples:**
```bash
# Node.js application
mirrord ci start --target deployment/api-server -- npm run start

# Python application
mirrord ci start --target pod/backend-abc123 -- python main.py

# With config file
mirrord ci start --config-file mirrord.json -- ./my-app

# Run in foreground (blocks until stopped)
mirrord ci start --foreground --target deployment/api -- npm start
```

### Stopping CI sessions

```bash
mirrord ci stop
```

This stops all running mirrord CI sessions. **Always run this after tests complete.**

### Multiple sessions

You can start multiple mirrord sessions in a single CI job:
```bash
mirrord ci start --target deployment/service-a -- ./service-a &
mirrord ci start --target deployment/service-b -- ./service-b &
# Run tests
npm test
# Stop all sessions
mirrord ci stop
```

## CI API Key (for mirrord Enterprise)

This only applies to the Enterprise plan; if you're on the open-source version of mirrord, skip this section. When the mirrord Operator is installed in the cluster, `mirrord ci` commands require a CI API key, and generating one requires an Enterprise plan. Without it, every CI runner registers as a new user and consumes a seat:

```bash
# Generate the key (run locally, not in CI)
mirrord ci api-key
```

Store this as a **secret** environment variable named `MIRRORD_CI_API_KEY` in your CI platform.

## Configuration

### CI-specific config options

```json
{
  "target": "deployment/my-app",
  "ci": {
    "output_dir": "/var/log/mirrord"
  }
}
```

| Option | Description | Default |
|--------|-------------|---------|
| `ci.output_dir` | Directory for stdout/stderr logs | OS temp dir (e.g., `/tmp/mirrord`) |

### Application logs

By default, application stdout/stderr are saved to:
```
/tmp/mirrord/<binary-name>-<unique-id>/
```

## CI Platform Examples

### GitHub Actions

```yaml
name: Integration Tests

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Set up kubeconfig
        run: |
          mkdir -p ~/.kube
          echo "${{ secrets.KUBECONFIG }}" | base64 -d > ~/.kube/config

      - name: Ensure mirrord is installed
        run: |
          # Use a pre-built runner image that includes mirrord, or install via your org's approved method.
          # See https://mirrord.dev/docs/overview/quick-start/ — do not pipe remote install scripts.
          mirrord --version

      - name: Start app with mirrord
        run: |
          mirrord ci start --target deployment/api-server -- npm run start
        env:
          MIRRORD_CI_API_KEY: ${{ secrets.MIRRORD_CI_API_KEY }}

      - name: Run tests
        run: npm test

      - name: Stop mirrord
        if: always()
        run: mirrord ci stop
```

### GitLab CI

```yaml
integration-tests:
  stage: test
  image: node:20
  before_script:
    - |
      # Install mirrord - use your organization's approved installation method
      # See https://mirrord.dev/docs/overview/quick-start/ for options
      # Option A: Pre-install mirrord in your CI Docker image
      # Option B: Use a pinned version from GitHub Releases with checksum verification
      mirrord --version  # Verify mirrord is available
    - mkdir -p ~/.kube
    - echo "$KUBECONFIG_CONTENT" | base64 -d > ~/.kube/config
  script:
    - mirrord ci start --target deployment/api-server -- npm run start
    - npm test
  after_script:
    - mirrord ci stop
  variables:
    MIRRORD_CI_API_KEY: $MIRRORD_CI_API_KEY
```

### CircleCI

```yaml
version: 2.1

jobs:
  integration-test:
    docker:
      - image: cimg/node:20.0
    steps:
      - checkout
      - run:
          name: Setup kubeconfig
          command: |
            mkdir -p ~/.kube
            echo "$KUBECONFIG_B64" | base64 -d > ~/.kube/config
      - run:
          name: Ensure mirrord is installed
          command: |
            # Pre-install mirrord in the image or use an org-approved install path (see mirrord docs).
            mirrord --version
      - run:
          name: Start mirrord CI session
          command: mirrord ci start --target deployment/api -- npm start
          environment:
            MIRRORD_CI_API_KEY: ${MIRRORD_CI_API_KEY}
      - run:
          name: Run tests
          command: npm test
      - run:
          name: Stop mirrord
          command: mirrord ci stop
          when: always
```

### Jenkins Pipeline

```groovy
pipeline {
    agent any

    environment {
        KUBECONFIG = credentials('kubeconfig-staging')
        MIRRORD_CI_API_KEY = credentials('mirrord-ci-api-key')
    }

    stages {
        stage('Setup') {
            steps {
                sh '''
                    # Ensure mirrord exists on the agent (pre-installed image or org-approved install)
                    mirrord --version
                '''
            }
        }

        stage('Test') {
            steps {
                sh 'mirrord ci start --target deployment/api -- npm start'
                sh 'npm test'
            }
            post {
                always {
                    sh 'mirrord ci stop'
                }
            }
        }
    }
}
```

## Kubernetes Access Setup

The CI runner must have access to your Kubernetes cluster. Common approaches:

### 1. Service Account (Recommended)

Create a dedicated service account for CI:

```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: ci-runner
  namespace: staging
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: mirrord-ci-role
  namespace: staging
rules:
- apiGroups: [""]
  resources: ["pods", "pods/log"]
  verbs: ["get", "list", "watch"]
- apiGroups: ["apps"]
  resources: ["deployments"]
  verbs: ["get", "list"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: mirrord-ci-binding
  namespace: staging
subjects:
- kind: ServiceAccount
  name: ci-runner
  namespace: staging
roleRef:
  kind: Role
  name: mirrord-ci-role
  apiGroup: rbac.authorization.k8s.io
```

### 2. Cloud Provider Authentication

- **GKE**: Use Workload Identity or service account key
- **EKS**: Use IAM roles for service accounts (IRSA)
- **AKS**: Use Azure AD pod identity or managed identity

## Common Issues

For detailed troubleshooting, refer to `references/troubleshooting.md`.

### CI-Specific Issues

| Issue | Solution |
|-------|----------|
| "kubectl not found" | Install kubectl in CI runner |
| "Cannot connect to cluster" | Check kubeconfig is properly configured |
| "mirrord ci start hangs" | Ensure target pod exists and is running |
| "Permission denied" | Check RBAC permissions for CI service account |
| "Session not stopping" | Use `mirrord ci stop` in `after_script` or `post` block |
| "Logs not found" | Check `ci.output_dir` config or default `/tmp/mirrord` |
| "Seats being consumed" | Set `MIRRORD_CI_API_KEY` environment variable |

### General mirrord Issues

| Issue | Solution |
|-------|----------|
| mirrord seems to have no effect | Binary may be statically linked. For Go: use `go build -ldflags='-linkmode external'` |
| Go DNS/outgoing filters not working | Build with `GODEBUG=netdns=cgo` |
| Traffic doesn't reach local process | Check port mapping - local may listen on different port than remote |
| Traffic stops reaching remote target | With service mesh, try `{"agent": {"flush_connections": false}}` |
| DNS resolution fails for K8s services | Change `feature.fs.mode` from `local` to `localwithoverrides` |
| Permission (EACCES) errors | Enable privileged mode: `{"agent": {"privileged": true}}` |
| Agent pods not cleaned up | Run: `kubectl delete jobs --selector=app=mirrord --field-selector=status.successful=1` |
| Certificate validation errors (macOS) | Use `{"experimental": {"trust_any_certificate": true}}` |
| Service mesh drops agent connection | Set static `agent.port` and add port exclusion in mesh config |

### Framework-Specific Issues

**Turbo (monorepo):**
```json
// turbo.json
{
  "globalPassThroughEnv": ["MIRRORD_*", "LD_PRELOAD", "DYLD_INSERT_LIBRARIES"]
}
```

**Remix/Vite/Next.js** - Override NODE_ENV to avoid production config:
```json
{
  "feature": {
    "env": {
      "override": {
        "NODE_ENV": "development"
      }
    }
  }
}
```

**Next.js with Nx** - Exclude conflicting variables:
```json
{
  "feature": {
    "env": {
      "exclude": ["NODE_ENV", "NX_NEXT_DIR"]
    }
  }
}
```

### Multi-Container Pods

If your pod has multiple containers, specify the target container explicitly:
```json
{
  "target": {
    "path": "pod/my-pod",
    "container": "my-app-container"
  }
}
```

## Response Guidelines

1. **Ask about their CI platform** - GitHub Actions, GitLab, CircleCI, Jenkins, etc.
2. **Verify prerequisites** - kubectl access, mirrord version
3. **Provide platform-specific examples** - Use the appropriate YAML/Groovy syntax
4. **Include cleanup steps** - Always show `mirrord ci stop` in appropriate hooks
5. **Mention API key** - Remind about `MIRRORD_CI_API_KEY` for Enterprise users

## Example Interaction

**User:** "How do I run my tests against staging in GitHub Actions?"

**Response:**
1. Ask: What's your target (deployment/pod name)?
2. Check: Do you have kubeconfig set up as a secret?
3. Provide: GitHub Actions workflow with:
   - kubeconfig setup
   - mirrord installation
   - `mirrord ci start` with their target
   - Test execution
   - `mirrord ci stop` in `if: always()` block
4. Mention: `MIRRORD_CI_API_KEY` if using the Enterprise plan

## Learn More

- [mirrord for CI Overview](https://metalbear.com/mirrord-for-ci/)
- [mirrord CI Documentation](https://mirrord.dev/docs/using-mirrord/mirrord-for-ci/)
- [mirrord Installation](https://mirrord.dev/docs/overview/quick-start/)
