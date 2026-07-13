#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

RUN_ID=${1:?usage: provision.sh <run-id> [kube-context]}
CONTEXT=${2:-${MIRRORD_E2E_CONTEXT:-gke_mirrord-test_us-central1-a_han-dev}}
NAMESPACE="ci-fe-${RUN_ID}"

kubectl --context "$CONTEXT" create namespace "$NAMESPACE"
kubectl --context "$CONTEXT" label namespace "$NAMESPACE" \
    mirrord-ci=frontend-e2e "run-id=${RUN_ID}"

kubectl --context "$CONTEXT" -n "$NAMESPACE" apply -f echo.yaml
kubectl --context "$CONTEXT" -n "$NAMESPACE" rollout status deployment/echo --timeout=120s

echo "namespace=${NAMESPACE}"
echo "target=deployment/echo"
echo "service=echo.${NAMESPACE}.svc.cluster.local"
