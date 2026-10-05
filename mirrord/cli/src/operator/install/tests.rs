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
spec:
  template:
    spec:
      containers:
        - name: mirrord-operator
          env:
            - name: OPERATOR_CLOUD_API_KEY
              value: "__MIRRORD_OPERATOR_API_KEY__"
"#;

/// The printed `helm install` and its API key lookup target the kubecontext of the installation
/// as one shell word, whatever its name is.
#[test]
fn summary_quotes_the_kubecontext() {
    let manifest = Manifest::parse(MANIFEST).unwrap();
    let version = semver::Version::new(3, 214, 0);

    let text = summary(&version, &manifest, Some("dev; rm -rf ~"));
    assert!(text.contains(" --kube-context 'dev; rm -rf ~' "), "{text}");
    assert!(
        text.contains("kubectl --context 'dev; rm -rf ~' -n"),
        "{text}"
    );

    let text = summary(&version, &manifest, Some("kind-dev"));
    assert!(text.contains(" --kube-context kind-dev "), "{text}");
    assert!(text.contains("kubectl --context kind-dev -n"), "{text}");

    let text = summary(&version, &manifest, None);
    assert!(text.contains("--kube-context").not(), "{text}");
    assert!(text.contains("kubectl -n"), "{text}");
}
