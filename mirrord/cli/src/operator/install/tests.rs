use std::ops::Not;

use super::{manifest::Manifest, summary};

const MANIFEST: &str = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: mirrord-operator
  namespace: mirrord
  labels:
    helm.sh/chart: mirrord-operator-1.35.0
    app.kubernetes.io/version: "3.197.0"
spec:
  template:
    spec:
      containers:
        - name: mirrord-operator
          env:
            - name: OPERATOR_CLOUD_API_KEY
              value: "__MIRRORD_OPERATOR_API_KEY__"
"#;

/// The printed uninstall command targets the kubecontext of the installation as one shell word.
#[test]
fn summary_quotes_the_kubecontext() {
    let manifest = Manifest::parse(MANIFEST).unwrap();
    let version = semver::Version::new(3, 214, 0);

    let text = summary(&version, &manifest, Some("dev; rm -rf ~"));
    assert!(
        text.contains("uninstall --context 'dev; rm -rf ~'`"),
        "{text}"
    );

    let text = summary(&version, &manifest, Some("kind-dev"));
    assert!(text.contains("uninstall --context kind-dev`"), "{text}");

    let text = summary(&version, &manifest, None);
    assert!(text.contains("--context").not(), "{text}");
    assert!(text.contains("helm").not(), "{text}");
}
