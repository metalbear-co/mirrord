//! What `preview start --resource` and `preview diff` print about a [`ResourcePlan`].

use std::fmt::Write;

use serde_json::Value;

use super::{ObjectRole, PlannedObject, ResourcePlan, Verdict, compare::FieldChange};

/// The informational lines `preview start --resource` prints before creating anything, one
/// entry per message.
pub(crate) fn summary(plan: &ResourcePlan<'_>) -> Vec<String> {
    let mut messages = Vec::new();

    match plan.target() {
        Some(target) if target.verdict == Verdict::Unchanged => messages.push(format!(
            "{} in {} matches the live spec, so the preview pod uses the target's live spec.",
            target.object.display(),
            target.object.source.display(),
        )),
        Some(target) => messages.push(format!(
            "Using {} from {} as the preview pod spec.",
            target.object.display(),
            target.object.source.display(),
        )),
        None => messages.push(format!(
            "No definition for {} found in the supplied manifests ({}).\n\
             The preview pod will use the target's live spec.",
            plan.target_display, plan.sources,
        )),
    }

    if !plan.planned.is_empty() {
        let total = plan.planned.len();
        let applied = plan
            .planned
            .iter()
            .filter(|planned| plan.is_applied(planned))
            .count();
        let skipped = total - applied;

        let mut block = format!(
            "Applying {applied} of {total} in-scope {} from {}",
            plural(total, "resource", "resources"),
            plan.sources,
        );
        if skipped > 0 {
            let _ = write!(block, " ({skipped} unchanged, skipped)");
        }
        block.push(':');
        for planned in &plan.planned {
            let _ = write!(
                block,
                "\n{} {}",
                planned.object.display(),
                verdict_label(plan, planned)
            );
        }
        messages.push(block);
    }

    if !plan.out_of_scope.is_empty() {
        messages.push(format!(
            "Skipped {} outside the target's scope: {}.",
            plural_count(plan.out_of_scope.len(), "resource", "resources"),
            plan.out_of_scope
                .iter()
                .map(|object| object.display())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    messages
}

/// The readable diff `mirrord preview diff` prints: one section per in-scope object, with a
/// line per changed field. Secret values are never printed, only whether each one changed.
pub(crate) fn render_diff(plan: &ResourcePlan<'_>) -> String {
    let mut output = String::new();

    if plan.target().is_none() {
        let _ = writeln!(
            output,
            "{}: no definition in {}, the preview pod would use the live spec",
            plan.target_display, plan.sources,
        );
    }

    for planned in &plan.planned {
        render_object(&mut output, plan, planned);
    }

    for object in &plan.out_of_scope {
        let _ = writeln!(
            output,
            "{} from {}: outside the target's scope, skipped",
            object.display(),
            object.source.display(),
        );
    }

    output
}

fn verdict_label(plan: &ResourcePlan<'_>, planned: &PlannedObject<'_>) -> &'static str {
    match planned.verdict {
        Verdict::Changed => "changed",
        Verdict::New => "new",
        // An unchanged Secret the live target does not use is still copied.
        Verdict::Unchanged if plan.is_applied(planned) => "unchanged, copied",
        Verdict::Unchanged => "unchanged, skipped",
    }
}

fn render_object(output: &mut String, plan: &ResourcePlan<'_>, planned: &PlannedObject<'_>) {
    let verdict = verdict_label(plan, planned);
    let _ = writeln!(
        output,
        "{} from {}: {verdict}",
        planned.object.display(),
        planned.object.source.display(),
    );

    let hide_values = planned.role == ObjectRole::Secret;
    for change in &planned.changes {
        // `data` itself changes as a whole when one side has no values at all.
        let hidden = hide_values && (change.path() == "data" || change.path().starts_with("data."));
        match change {
            FieldChange::Added { path, value } => {
                let _ = writeln!(
                    output,
                    "  + {path}: {}",
                    shown(value, hidden, "(value hidden)")
                );
            }
            FieldChange::Removed { path, value } => {
                let _ = writeln!(
                    output,
                    "  - {path}: {}",
                    shown(value, hidden, "(value hidden)")
                );
            }
            FieldChange::Changed { path, .. } if hidden => {
                let _ = writeln!(output, "  ~ {path}: (value hidden, changed)");
            }
            FieldChange::Changed { path, old, new } => {
                let _ = writeln!(output, "  ~ {path}");
                let _ = writeln!(output, "      - {old}");
                let _ = writeln!(output, "      + {new}");
            }
        }
    }
}

fn shown(value: &Value, hidden: bool, placeholder: &str) -> String {
    if hidden {
        placeholder.to_owned()
    } else {
        value.to_string()
    }
}

fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

fn plural_count(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", plural(count, one, many))
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::Path};

    use serde_json::json;

    use super::*;
    use crate::preview::resources::SuppliedObject;

    fn object(kind: &str, name: &str, source: &str) -> SuppliedObject {
        SuppliedObject::new(
            Path::new(source),
            0,
            json!({"kind": kind, "metadata": {"name": name}}),
        )
        .unwrap()
    }

    fn planned<'a>(
        object: &'a SuppliedObject,
        role: ObjectRole,
        verdict: Verdict,
        changes: Vec<FieldChange>,
    ) -> PlannedObject<'a> {
        PlannedObject {
            object,
            role,
            verdict,
            changes,
        }
    }

    /// The example from INT-699: two changed or new objects applied, one unchanged skipped.
    #[test]
    fn summary_matches_the_documented_messages() {
        let deployment = object(
            "Deployment",
            "qa-workflowsvc",
            "./k8s/workflowsvc-deployment.yaml",
        );
        let config_map = object(
            "ConfigMap",
            "qa-workflowsvc-1.0.0-318.1319-gh-qa",
            "./k8s/configmap.yaml",
        );
        let secret = object("Secret", "qa-workflowsvc-tls", "./k8s/secret.yaml");
        let ingress = object("Ingress", "web", "./k8s/ingress.yaml");

        let plan = ResourcePlan {
            sources: "./k8s/".to_owned(),
            target_display: "deployment/qa-workflowsvc".to_owned(),
            planned: vec![
                planned(&deployment, ObjectRole::Target, Verdict::Changed, vec![]),
                planned(&config_map, ObjectRole::ConfigMap, Verdict::New, vec![]),
                planned(&secret, ObjectRole::Secret, Verdict::Unchanged, vec![]),
            ],
            out_of_scope: vec![&ingress],
            notes: vec![],
            // The live target already uses this Secret, so the unchanged copy is not sent.
            live_secrets: BTreeSet::from(["qa-workflowsvc-tls".to_owned()]),
        };

        assert_eq!(
            summary(&plan),
            [
                "Using deployment/qa-workflowsvc from ./k8s/workflowsvc-deployment.yaml as the \
                 preview pod spec.",
                "Applying 2 of 3 in-scope resources from ./k8s/ (1 unchanged, skipped):\n\
                 deployment/qa-workflowsvc changed\n\
                 configmap/qa-workflowsvc-1.0.0-318.1319-gh-qa new\n\
                 secret/qa-workflowsvc-tls unchanged, skipped",
                "Skipped 1 resource outside the target's scope: ingress/web.",
            ]
        );
    }

    /// An unchanged Secret the live target does not use is copied, so it counts as applied.
    #[test]
    fn unchanged_secret_the_live_target_does_not_use_is_counted_as_copied() {
        let secret = object("Secret", "newly-used", "./k8s/secret.yaml");
        let plan = ResourcePlan {
            sources: "./k8s/".to_owned(),
            target_display: "deployment/app".to_owned(),
            planned: vec![planned(
                &secret,
                ObjectRole::Secret,
                Verdict::Unchanged,
                vec![],
            )],
            out_of_scope: vec![],
            notes: vec![],
            live_secrets: BTreeSet::new(),
        };

        assert_eq!(
            summary(&plan),
            [
                "No definition for deployment/app found in the supplied manifests (./k8s/).\n\
              The preview pod will use the target's live spec.",
                "Applying 1 of 1 in-scope resource from ./k8s/:\n\
              secret/newly-used unchanged, copied"
            ]
        );
        assert!(
            render_diff(&plan)
                .contains("secret/newly-used from ./k8s/secret.yaml: unchanged, copied")
        );
    }

    #[test]
    fn summary_says_when_the_target_is_not_in_the_files() {
        let plan = ResourcePlan {
            sources: "./k8s/".to_owned(),
            target_display: "deployment/app".to_owned(),
            planned: vec![],
            out_of_scope: vec![],
            notes: vec![],
            live_secrets: Default::default(),
        };

        assert_eq!(
            summary(&plan),
            [
                "No definition for deployment/app found in the supplied manifests (./k8s/).\n\
              The preview pod will use the target's live spec."
            ]
        );
    }

    #[test]
    fn summary_for_an_unchanged_target_says_the_live_spec_is_used() {
        let deployment = object("Deployment", "app", "./k8s/app.yaml");
        let plan = ResourcePlan {
            sources: "./k8s/".to_owned(),
            target_display: "deployment/app".to_owned(),
            planned: vec![planned(
                &deployment,
                ObjectRole::Target,
                Verdict::Unchanged,
                vec![],
            )],
            out_of_scope: vec![],
            notes: vec![],
            live_secrets: Default::default(),
        };

        assert_eq!(
            summary(&plan),
            [
                "deployment/app in ./k8s/app.yaml matches the live spec, so the preview pod uses \
                 the target's live spec.",
                "Applying 0 of 1 in-scope resource from ./k8s/ (1 unchanged, skipped):\n\
                 deployment/app unchanged, skipped",
            ]
        );
    }

    /// A Secret's values never reach the terminal, whether added, removed, or changed; other
    /// fields show old and new values.
    #[test]
    fn diff_shows_field_changes_and_hides_secret_values() {
        let deployment = object("Deployment", "app", "./k8s/app.yaml");
        let secret = object("Secret", "creds", "./k8s/secret.yaml");
        let plan = ResourcePlan {
            sources: "./k8s/".to_owned(),
            target_display: "deployment/app".to_owned(),
            planned: vec![
                planned(
                    &deployment,
                    ObjectRole::Target,
                    Verdict::Changed,
                    vec![FieldChange::Changed {
                        path: "spec.template.spec.containers[app].env[LOG_LEVEL].value".to_owned(),
                        old: json!("info"),
                        new: json!("debug"),
                    }],
                ),
                planned(
                    &secret,
                    ObjectRole::Secret,
                    Verdict::Changed,
                    vec![
                        FieldChange::Changed {
                            path: "data.password".to_owned(),
                            old: json!("aHVudGVyMg=="),
                            new: json!("aHVudGVyMw=="),
                        },
                        FieldChange::Added {
                            path: "data.token".to_owned(),
                            value: json!("c2VjcmV0"),
                        },
                        // One side with no values at all changes `data` as a whole.
                        FieldChange::Removed {
                            path: "data".to_owned(),
                            value: json!({"old": "b2xkLXNlY3JldA=="}),
                        },
                    ],
                ),
            ],
            out_of_scope: vec![],
            notes: vec![],
            live_secrets: Default::default(),
        };

        let rendered = render_diff(&plan);

        assert_eq!(
            rendered,
            "\
deployment/app from ./k8s/app.yaml: changed
  ~ spec.template.spec.containers[app].env[LOG_LEVEL].value
      - \"info\"
      + \"debug\"
secret/creds from ./k8s/secret.yaml: changed
  ~ data.password: (value hidden, changed)
  + data.token: (value hidden)
  - data: (value hidden)
"
        );
        assert!(!rendered.contains("b2xkLXNlY3JldA"));
        assert!(!rendered.contains("aHVudGVy"));
        assert!(!rendered.contains("c2VjcmV0"));
    }
}
