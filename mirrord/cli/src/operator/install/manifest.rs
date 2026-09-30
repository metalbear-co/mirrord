//! The operator manifest: the mirrord-operator helm chart rendered with default values by charts
//! CI (`render_default_manifest.sh` in the charts repo) and published next to each chart release.

use std::{collections::HashMap, ops::Not, path::Path};

use kube::{ResourceExt, api::DynamicObject};
use serde::Deserialize;
use serde_json::Value;

use super::error::OperatorInstallError;

pub(super) const CHARTS_REPO_URL: &str = "https://metalbear-co.github.io/charts";

pub(super) const CHART_NAME: &str = "mirrord-operator";

/// The helm release the objects are attributed to, matching the documented `helm install`.
pub(super) const RELEASE_NAME: &str = "mirrord-operator";

/// Stands in for the operator API key in the rendered manifest.
///
/// Set by `render_default_manifest.sh` in the charts repo, the two must stay in sync.
const API_KEY_PLACEHOLDER: &str = "__MIRRORD_OPERATOR_API_KEY__";

/// The operator Deployment env var the chart puts `cloud.apiKey.key` in.
const API_KEY_ENV: &str = "OPERATOR_CLOUD_API_KEY";

/// helm only creates objects with this annotation at their point in the release lifecycle. Created
/// directly, they would run right away, e.g. the chart's pre-delete cleanup Job deletes the
/// operator's CRDs and Deployment.
const HOOK_ANNOTATION: &str = "helm.sh/hook";

/// Together with [`MANAGED_BY_LABEL`], ties an object to a helm release, so `helm install` adopts
/// it instead of failing because it already exists.
const RELEASE_NAME_ANNOTATION: &str = "meta.helm.sh/release-name";

const RELEASE_NAMESPACE_ANNOTATION: &str = "meta.helm.sh/release-namespace";

/// Names the chart and version an object was rendered from, as `<chart>-<version>` with `+`
/// replaced by `_`.
const CHART_LABEL: &str = "helm.sh/chart";

/// Required by helm to adopt an object. Most chart templates already set it, but not all of them
/// (e.g. the CRDs).
const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";

/// The parts of a helm repository `index.yaml` needed to find the latest chart version.
#[derive(Deserialize)]
struct ChartIndex {
    entries: HashMap<String, Vec<ChartIndexEntry>>,
}

#[derive(Deserialize)]
struct ChartIndexEntry {
    version: String,
}

/// Finds the latest stable mirrord-operator chart version, the one `helm install` picks without
/// `--version`.
pub(super) async fn latest_chart_version(
    http: &reqwest::Client,
) -> Result<semver::Version, OperatorInstallError> {
    let url = format!("{CHARTS_REPO_URL}/index.yaml");
    let index = fetch(http, &url)
        .await?
        .ok_or_else(|| OperatorInstallError::NoChartVersion { url: url.clone() })?;
    let index = serde_saphyr::from_str(&index).map_err(OperatorInstallError::ParseIndex)?;

    latest_version(index).ok_or(OperatorInstallError::NoChartVersion { url })
}

fn latest_version(mut index: ChartIndex) -> Option<semver::Version> {
    index
        .entries
        .remove(CHART_NAME)?
        .into_iter()
        .filter_map(|entry| semver::Version::parse(&entry.version).ok())
        .filter(|version| version.pre.is_empty())
        .max()
}

/// Fetches the rendered manifest of the given chart version.
///
/// The manifest is uploaded to the chart's release after the charts
/// index already lists it, so a missing manifest for the latest
/// version means publishing probably is still in progress.
pub(super) async fn fetch_manifest(
    http: &reqwest::Client,
    version: semver::Version,
) -> Result<String, OperatorInstallError> {
    let url = format!(
        "https://github.com/metalbear-co/charts/releases/download/\
        {CHART_NAME}-{version}/{CHART_NAME}-{version}.yaml"
    );

    fetch(http, &url)
        .await?
        .ok_or(OperatorInstallError::ManifestNotPublished { version })
}

pub(super) fn read_manifest(path: &Path) -> Result<String, OperatorInstallError> {
    std::fs::read_to_string(path).map_err(|source| OperatorInstallError::ReadManifest {
        path: path.to_owned(),
        source,
    })
}

/// GETs `url`, returning `None` if it does not exist.
async fn fetch(http: &reqwest::Client, url: &str) -> Result<Option<String>, OperatorInstallError> {
    let fetch_error = |source| OperatorInstallError::Fetch {
        url: url.to_owned(),
        source,
    };

    let response = http.get(url).send().await.map_err(fetch_error)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    response
        .error_for_status()
        .map_err(fetch_error)?
        .text()
        .await
        .map(Some)
        .map_err(fetch_error)
}

/// The objects of the operator manifest, in the order helm installs them.
#[derive(Debug)]
pub(super) struct Manifest {
    objects: Vec<DynamicObject>,
    /// Version of the chart the manifest was rendered from, which may differ from the operator's.
    chart_version: semver::Version,
    operator_deployment: String,
    /// Namespace of the operator Deployment.
    operator_namespace: String,
}

impl Manifest {
    /// Parses a rendered manifest, leaving out helm hooks.
    ///
    /// Also validates that the API key can be set, so a broken manifest fails before a trial is
    /// started for it.
    pub(super) fn parse(manifest: &str) -> Result<Self, OperatorInstallError> {
        let mut objects = serde_saphyr::from_multiple::<Option<DynamicObject>>(manifest)
            .map_err(OperatorInstallError::ParseManifest)?
            .into_iter()
            .flatten()
            .filter(|object| object.annotations().contains_key(HOOK_ANNOTATION).not())
            .collect::<Vec<_>>();

        let mut placeholders = 0;
        for object in &mut objects {
            for_each_placeholder(&mut object.data, &mut |_| placeholders += 1);
        }
        if placeholders != 1 {
            return Err(OperatorInstallError::ApiKeyPlaceholder(placeholders));
        }

        let deployment = objects
            .iter()
            .find(|object| {
                object
                    .types
                    .as_ref()
                    .is_some_and(|types| types.kind == "Deployment")
            })
            .ok_or(OperatorInstallError::NoDeployment)?;
        let chart_version = deployment
            .labels()
            .get(CHART_LABEL)
            .and_then(|chart| chart.strip_prefix(&format!("{CHART_NAME}-")))
            .and_then(|version| semver::Version::parse(&version.replace('_', "+")).ok())
            .ok_or(OperatorInstallError::NoChartVersionLabel)?;
        let operator_deployment = deployment.name_any();
        let operator_namespace = deployment
            .namespace()
            .ok_or(OperatorInstallError::NoDeployment)?;

        Ok(Self {
            objects,
            chart_version,
            operator_deployment,
            operator_namespace,
        })
    }

    pub(super) fn objects(&self) -> &[DynamicObject] {
        &self.objects
    }

    pub(super) fn operator_namespace(&self) -> &str {
        &self.operator_namespace
    }

    pub(super) fn chart_version(&self) -> &semver::Version {
        &self.chart_version
    }

    /// A shell command substitution that reads the installed operator's API key from the cluster.
    ///
    /// Lets users move the installation to helm with the key it already uses, which they may have
    /// never seen, e.g. when it came from a trial signup.
    pub(super) fn api_key_lookup(&self) -> String {
        format!(
            "$(kubectl -n {} get deployment {} -o \
            jsonpath='{{.spec.template.spec.containers[*].env[?(@.name==\"{API_KEY_ENV}\")].value}}')",
            self.operator_namespace, self.operator_deployment,
        )
    }

    /// Attributes all objects to the [`RELEASE_NAME`] helm release in `release_namespace`, so
    /// users can move the installation to helm by running `helm install`.
    pub(super) fn attribute_to_release(&mut self, release_namespace: &str) {
        for object in &mut self.objects {
            object
                .labels_mut()
                .insert(MANAGED_BY_LABEL.to_owned(), "Helm".to_owned());

            let annotations = object.annotations_mut();
            annotations.insert(RELEASE_NAME_ANNOTATION.to_owned(), RELEASE_NAME.to_owned());
            annotations.insert(
                RELEASE_NAMESPACE_ANNOTATION.to_owned(),
                release_namespace.to_owned(),
            );
        }
    }

    pub(super) fn set_api_key(&mut self, api_key: &str) {
        for object in &mut self.objects {
            for_each_placeholder(&mut object.data, &mut |value| api_key.clone_into(value));
        }
    }
}

fn for_each_placeholder(value: &mut Value, f: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(string) if string == API_KEY_PLACEHOLDER => f(string),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| for_each_placeholder(item, f)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|field| for_each_placeholder(field, f)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"---
# Source: mirrord-operator/templates/namespace.yaml
apiVersion: v1
kind: Namespace
metadata:
  name: mirrord
---
# Source: mirrord-operator/templates/deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: mirrord-operator
  namespace: mirrord
  labels:
    helm.sh/chart: mirrord-operator-1.35.0
spec:
  template:
    spec:
      containers:
        - name: mirrord-operator
          env:
            - name: OPERATOR_CLOUD_API_KEY
              value: "__MIRRORD_OPERATOR_API_KEY__"
---
# Source: mirrord-operator/templates/hooks.yaml
apiVersion: batch/v1
kind: Job
metadata:
  name: mirrord-cleanup
  namespace: mirrord
  annotations:
    helm.sh/hook: pre-delete
"#;

    #[test]
    fn parse_skips_hooks_and_empty_documents() {
        let manifest = Manifest::parse(MANIFEST).unwrap();

        let names = manifest
            .objects()
            .iter()
            .map(|object| object.name_any())
            .collect::<Vec<_>>();
        assert_eq!(names, ["mirrord", "mirrord-operator"]);
        assert_eq!(manifest.operator_namespace(), "mirrord");
        assert_eq!(manifest.chart_version(), &semver::Version::new(1, 35, 0));
    }

    #[test]
    fn parse_requires_exactly_one_placeholder() {
        let missing = MANIFEST.replace(API_KEY_PLACEHOLDER, "key");
        assert!(matches!(
            Manifest::parse(&missing),
            Err(OperatorInstallError::ApiKeyPlaceholder(0))
        ));

        let duplicated = format!(
            "{MANIFEST}---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: config\n\
            data:\n  key: {API_KEY_PLACEHOLDER}\n"
        );
        assert!(matches!(
            Manifest::parse(&duplicated),
            Err(OperatorInstallError::ApiKeyPlaceholder(2))
        ));
    }

    #[test]
    fn set_api_key_replaces_placeholder() {
        let mut manifest = Manifest::parse(MANIFEST).unwrap();
        manifest.set_api_key("secret");

        let api_key = manifest
            .objects()
            .get(1)
            .and_then(|deployment| {
                deployment
                    .data
                    .pointer("/spec/template/spec/containers/0/env/0/value")
            })
            .and_then(Value::as_str);
        assert_eq!(api_key, Some("secret"));
    }

    #[test]
    fn attribute_to_release_marks_all_objects() {
        let mut manifest = Manifest::parse(MANIFEST).unwrap();
        manifest.attribute_to_release("default");

        for object in manifest.objects() {
            assert_eq!(
                object.labels().get(MANAGED_BY_LABEL).map(String::as_str),
                Some("Helm")
            );

            let annotations = object.annotations();
            assert_eq!(
                annotations.get(RELEASE_NAME_ANNOTATION).map(String::as_str),
                Some(RELEASE_NAME)
            );
            assert_eq!(
                annotations
                    .get(RELEASE_NAMESPACE_ANNOTATION)
                    .map(String::as_str),
                Some("default")
            );
        }
    }

    #[test]
    fn latest_version_ignores_prereleases_and_other_charts() {
        let index = serde_saphyr::from_str(
            r#"
apiVersion: v1
entries:
  mirrord-operator:
    - version: 3.9.0
    - version: 3.10.0
    - version: 3.11.0-rc.1
  mirrord-operator-license-server:
    - version: 4.0.0
"#,
        )
        .unwrap();

        assert_eq!(latest_version(index), Some(semver::Version::new(3, 10, 0)));
    }
}
