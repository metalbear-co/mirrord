#!/usr/bin/env bash
set -euo pipefail

RUN_ID=${1:?usage: teardown.sh <run-id> [kube-context]}
CONTEXT=${2:-${MIRRORD_E2E_CONTEXT:-gke_mirrord-test_us-central1-a_han-dev}}
NAMESPACE="ci-fe-${RUN_ID}"

kubectl --context "$CONTEXT" delete namespace "$NAMESPACE" --ignore-not-found --wait=false
