//! `--chart`: rendering a Helm chart into the objects the `--resource` pipeline works on.
//!
//! Helm templates are Go templates with Sprig functions, which no Rust crate renders, so the
//! chart goes through the `helm` binary on `PATH` (`helm template`) and its output is parsed
//! like a manifest file. Everything runs on this machine: `helm template` never contacts the
//! cluster.

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::Stdio,
};

use mirrord_command::resolve_tokio_command;

use super::{ResourcesError, SuppliedObject, manifest};

/// Helm prints this in front of every rendered document, naming the template inside the chart
/// (`mychart/templates/deployment.yaml`).
const SOURCE_COMMENT: &str = "# Source: ";

/// A chart to render, as the user described it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HelmChart {
    /// Chart directory or packaged `.tgz`.
    pub chart: PathBuf,
    /// Values files, in `helm template -f` order.
    pub values: Vec<PathBuf>,
    /// `.Release.Name`; the target's name when not set.
    pub release_name: Option<String>,
}

/// Renders `chart` as release `release_name` in `namespace` into objects.
///
/// The chart and values paths are checked here first, so a typo gets the same message a
/// missing `--resource` path gets instead of helm's.
pub(crate) async fn render(
    chart: &HelmChart,
    release_name: &str,
    namespace: &str,
) -> Result<Vec<SuppliedObject>, ResourcesError> {
    for path in std::iter::once(&chart.chart).chain(&chart.values) {
        if !path.exists() {
            return Err(ResourcesError::PathMissing(path.clone()));
        }
    }

    let args = template_args(chart, release_name, namespace);
    let output = resolve_tokio_command("helm")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => ResourcesError::HelmNotFound,
            _ => ResourcesError::HelmRun { source },
        })?;

    if !output.status.success() {
        return Err(ResourcesError::HelmFailed {
            chart: chart.chart.clone(),
            command: display_command(&args),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    let objects = parse_rendered(&chart.chart, &String::from_utf8_lossy(&output.stdout))?;
    if objects.is_empty() {
        return Err(ResourcesError::ChartRenderedNothing(chart.chart.clone()));
    }
    Ok(objects)
}

/// The `helm template` arguments for `chart`.
fn template_args(chart: &HelmChart, release_name: &str, namespace: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "template".into(),
        release_name.into(),
        chart.chart.as_os_str().to_owned(),
        "--namespace".into(),
        namespace.into(),
    ];
    for values in &chart.values {
        args.push("--values".into());
        args.push(values.as_os_str().to_owned());
    }
    args
}

/// `helm template my-app ./chart --namespace qa --values ./values.yaml`, for messages.
fn display_command(args: &[OsString]) -> String {
    std::iter::once("helm".to_owned())
        .chain(args.iter().map(|arg| arg.to_string_lossy().into_owned()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses `helm template` output into objects, each attributed to the template it came from.
///
/// Helm separates the rendered documents with `---` lines and puts a `# Source:` comment in
/// front of each, which the YAML parser would drop, so the documents are split here first and
/// parsed one at a time.
fn parse_rendered(chart: &Path, rendered: &str) -> Result<Vec<SuppliedObject>, ResourcesError> {
    let mut objects = Vec::new();
    for document in split_documents(rendered) {
        let source = document
            .lines()
            .find_map(|line| line.strip_prefix(SOURCE_COMMENT))
            .map_or_else(
                || chart.to_path_buf(),
                |template| template_path(chart, template),
            );
        let parsed = manifest::parse(&source, &document).map_err(|error| match error {
            ResourcesError::Parse {
                path,
                location,
                message,
            } => ResourcesError::RenderedTemplateInvalid {
                template: path,
                location,
                message,
            },
            other => other,
        })?;
        objects.extend(parsed);
    }
    Ok(objects)
}

/// The non-empty documents of `rendered`, split at the `---` lines helm puts between them. Only
/// a line holding just `---` separates: inside a document a `---` is always indented or quoted.
fn split_documents(rendered: &str) -> Vec<String> {
    let mut documents = Vec::new();
    let mut current = String::new();
    for line in rendered.lines() {
        if line.trim_end() == "---" {
            documents.push(std::mem::take(&mut current));
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    documents.push(current);
    documents.retain(|document| !document.trim().is_empty());
    documents
}

/// `mychart/templates/deployment.yaml` as helm names it, shown as the path under the chart the
/// user passed (`./chart/templates/deployment.yaml`): helm's first segment is the chart's name
/// from `Chart.yaml`, not its directory.
fn template_path(chart: &Path, template: &str) -> PathBuf {
    match template.split_once('/') {
        Some((_, inside_chart)) => chart.join(inside_chart),
        None => chart.join(template),
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // Tests read rendered objects with `objects[0]`; a panic just fails the test.
mod tests {
    use std::fs;

    use super::*;

    const RENDERED: &str = "\
---
# Source: echo/templates/configmap.yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: echo-config
data:
  A: \"1\"
---
# Source: echo/templates/deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: echo
spec:
  template:
    spec:
      containers:
        - name: echo
          args: [\"--marker\", \"---\"]
";

    #[test]
    fn rendered_documents_are_split_and_attributed_to_their_templates() {
        let objects = parse_rendered(Path::new("./chart"), RENDERED).unwrap();

        let seen: Vec<_> = objects
            .iter()
            .map(|object| (object.display(), object.source.display().to_string()))
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "configmap/echo-config".to_owned(),
                    "./chart/templates/configmap.yaml".to_owned()
                ),
                (
                    "deployment/echo".to_owned(),
                    "./chart/templates/deployment.yaml".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn documents_without_a_source_comment_are_attributed_to_the_chart() {
        let objects = parse_rendered(
            Path::new("./chart"),
            "kind: ConfigMap\nmetadata: {name: a}\n",
        )
        .unwrap();
        assert_eq!(objects[0].source, Path::new("./chart"));
    }

    #[test]
    fn invalid_rendered_yaml_names_the_template_not_a_file() {
        let error = parse_rendered(
            Path::new("./chart"),
            "---\n# Source: echo/templates/bad.yaml\nkind: ConfigMap\nmetadata:\n  name: a\n  b: c: d\n",
        )
        .unwrap_err();

        assert!(
            matches!(
                &error,
                ResourcesError::RenderedTemplateInvalid { template, location: Some((5, _)), .. }
                    if template == Path::new("./chart/templates/bad.yaml")
            ),
            "{error:?}"
        );
        assert!(error.to_string().contains("rendered output"), "{error}");
    }

    #[test]
    fn template_args_pass_release_namespace_and_values_in_order() {
        let chart = HelmChart {
            chart: PathBuf::from("./chart"),
            values: vec![PathBuf::from("./base.yaml"), PathBuf::from("./qa.yaml")],
            release_name: None,
        };

        let args = template_args(&chart, "my-app", "qa");

        assert_eq!(
            display_command(&args),
            "helm template my-app ./chart --namespace qa --values ./base.yaml --values ./qa.yaml"
        );
    }

    #[tokio::test]
    async fn missing_values_file_is_reported_before_helm_runs() {
        let dir = tempfile::tempdir().unwrap();
        let chart = HelmChart {
            chart: dir.path().to_path_buf(),
            values: vec![dir.path().join("nowhere.yaml")],
            release_name: None,
        };

        let error = render(&chart, "app", "default").await.unwrap_err();

        assert!(
            matches!(&error, ResourcesError::PathMissing(path) if path == &dir.path().join("nowhere.yaml")),
            "{error}"
        );
    }

    /// Runs the real `helm` when it is installed. The release name and namespace have to reach
    /// `.Release`, and a values file has to win over the chart's `values.yaml`, or the rendered
    /// objects do not match what is live.
    #[tokio::test]
    async fn real_helm_renders_release_namespace_and_values() {
        if which::which("helm").is_err() {
            eprintln!("skipping: helm is not installed");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let chart_dir = dir.path().join("chart");
        fs::create_dir_all(chart_dir.join("templates")).unwrap();
        fs::write(
            chart_dir.join("Chart.yaml"),
            "apiVersion: v2\nname: echo\nversion: 0.1.0\n",
        )
        .unwrap();
        fs::write(chart_dir.join("values.yaml"), "marker: from-chart\n").unwrap();
        fs::write(
            chart_dir.join("templates/configmap.yaml"),
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {{ .Release.Name }}-config\n  \
             namespace: {{ .Release.Namespace }}\ndata:\n  MARKER: {{ .Values.marker | quote }}\n",
        )
        .unwrap();
        let values = dir.path().join("values-qa.yaml");
        fs::write(&values, "marker: from-values\n").unwrap();

        let defaults = render(
            &HelmChart {
                chart: chart_dir.clone(),
                values: vec![],
                release_name: None,
            },
            "my-app",
            "qa",
        )
        .await
        .unwrap();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].name, "my-app-config");
        assert_eq!(defaults[0].namespace.as_deref(), Some("qa"));
        assert_eq!(defaults[0].value["data"]["MARKER"], "from-chart");
        assert_eq!(
            defaults[0].source,
            chart_dir.join("templates/configmap.yaml")
        );

        let overridden = render(
            &HelmChart {
                chart: chart_dir,
                values: vec![values],
                release_name: None,
            },
            "my-app",
            "qa",
        )
        .await
        .unwrap();
        assert_eq!(overridden[0].value["data"]["MARKER"], "from-values");
    }

    #[tokio::test]
    async fn helm_failure_quotes_the_command_and_helms_message() {
        if which::which("helm").is_err() {
            eprintln!("skipping: helm is not installed");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let chart_dir = dir.path().join("chart");
        fs::create_dir_all(chart_dir.join("templates")).unwrap();
        fs::write(
            chart_dir.join("Chart.yaml"),
            "apiVersion: v2\nname: echo\nversion: 0.1.0\n",
        )
        .unwrap();
        fs::write(
            chart_dir.join("templates/broken.yaml"),
            "kind: {{ .Values.missing.deep }}\n",
        )
        .unwrap();

        let error = render(
            &HelmChart {
                chart: chart_dir.clone(),
                values: vec![],
                release_name: None,
            },
            "my-app",
            "qa",
        )
        .await
        .unwrap_err();

        let rendered = error.to_string();
        assert!(
            rendered.contains(&format!("helm template my-app {}", chart_dir.display())),
            "{rendered}"
        );
        assert!(rendered.contains("templates/broken.yaml"), "{rendered}");
        assert!(rendered.ends_with("Nothing was created."), "{rendered}");
    }
}
