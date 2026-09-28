//! `--resource`: building a preview from the user's Kubernetes manifests instead of the live
//! target.
//!
//! The pipeline, shared by `mirrord preview start --resource` and `mirrord preview diff`:
//!
//! 1. [`manifest::load`] reads the YAML files into [`SuppliedObject`]s, on this machine.
//! 2. [`scope::select`] keeps the target and the ConfigMaps and Secrets its pod reads.
//! 3. [`plan`] compares each of those with the live cluster ([`compare`]).
//! 4. [`plan`] also validates every changed object with a server-side dry run, so a spec the
//!    cluster would reject fails the command before anything is created.
//!
//! `preview start` then turns the plan into the session's `spec.specResources`
//! ([`ResourcePlan::spec_resources`]); the operator builds the preview pod from it. `preview
//! diff` prints the plan instead ([`report::render_diff`]).
//!
//! Steps 3 and 4 run in the CLI, with the user's own credentials, on purpose. The operator
//! never reads user-owned Secrets, and a comparison it made on the user's behalf would tell
//! anyone allowed to start a preview whether a guessed Secret value is right. Here the user
//! can only compare what they could already read.
//!
//! Everything after step 1 works on [`SuppliedObject`]s and never looks at where they came
//! from beyond [`SuppliedObject::source`], the label messages print. Another manifest source
//! (a Helm chart rendered to documents, for example) plugs in by producing the same objects.

mod compare;
mod manifest;
mod report;
mod scope;

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use k8s_openapi::{
    ByteString,
    api::core::v1::{ConfigMap, Secret},
};
use kube::{
    Api, Client,
    api::{ApiResource, DynamicObject, GroupVersionKind, PostParams},
};
use miette::Diagnostic;
use mirrord_operator::crd::{
    preview::{PreviewSpecConfigMap, PreviewSpecResources, PreviewSpecSecret},
    session::KubeResourceTarget,
};
use serde_json::{Map, Value, json};
use thiserror::Error;

use self::{
    compare::{FieldChange, SecretDecodeError},
    scope::TargetRef,
};
pub(crate) use self::{
    manifest::load,
    report::{render_diff, summary},
};

/// One Kubernetes object from the user's manifests.
#[derive(Clone, Debug)]
pub(crate) struct SuppliedObject {
    /// Where the object came from, as messages show it (`./k8s/configmap.yaml`).
    pub source: PathBuf,
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    pub value: Value,
}

impl SuppliedObject {
    /// Wraps the `index`-th document of `source`, which must name its `kind` and
    /// `metadata.name` like every object `kubectl apply` accepts.
    pub fn new(source: &Path, index: usize, value: Value) -> Result<Self, ResourcesError> {
        let field = |pointer: &str| value.pointer(pointer).and_then(Value::as_str);

        let (Some(kind), Some(name)) = (field("/kind"), field("/metadata/name")) else {
            return Err(ResourcesError::NotAnObject {
                path: source.to_path_buf(),
                index,
            });
        };

        Ok(Self {
            source: source.to_path_buf(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            namespace: field("/metadata/namespace").map(str::to_owned),
            value,
        })
    }

    /// `configmap/app-config`, the form used in every message.
    pub fn display(&self) -> String {
        format!("{}/{}", self.kind.to_lowercase(), self.name)
    }
}

/// Everything that stops `--resource` before anything is created.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum ResourcesError {
    #[error("Path {} does not exist.", .0.display())]
    #[diagnostic(help(
        "Pass manifest files or directories of them with `--resource`, or fix \
         `feature.preview.spec_resources` in your mirrord config."
    ))]
    PathMissing(PathBuf),

    #[error("No Kubernetes manifests found in {} (looked for *.yaml, *.yml).", .0.display())]
    #[diagnostic(help(
        "Only files directly in the directory are read, not subdirectories. Pass the directory \
         that holds the manifests, or the files themselves."
    ))]
    NoManifests(PathBuf),

    #[error("No Kubernetes objects found in {0}.\nNothing was created.")]
    #[diagnostic(help(
        "The files hold only empty documents. Add the manifests to preview, or leave out \
         `--resource` to preview the live spec."
    ))]
    NoObjects(String),

    #[error(
        "{object} from {} is a `{secret_type}` Secret, which a preview cannot copy.\nNothing \
         was created.",
        source_path.display()
    )]
    #[diagnostic(help(
        "Leave this Secret out of the manifests: the preview then uses the live one."
    ))]
    UnsupportedSecretType {
        object: String,
        source_path: PathBuf,
        secret_type: String,
    },

    #[error("{} is not a YAML file.", .0.display())]
    #[diagnostic(help("Only `*.yaml` and `*.yml` files are read as Kubernetes manifests."))]
    NotYaml(PathBuf),

    #[error("Failed to read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(
        "Failed to parse {}: {}{message}\nNothing was created.",
        path.display(),
        location.map(|(line, column)| format!("line {line}, column {column}: ")).unwrap_or_default(),
    )]
    #[diagnostic(help("Fix the YAML and run the command again."))]
    Parse {
        path: PathBuf,
        /// Line and column, both starting at 1.
        location: Option<(u64, u64)>,
        message: String,
    },

    #[error(
        "Document {} of {} is not a Kubernetes object: it needs a `kind` and a `metadata.name`.\n\
         Nothing was created.",
        index + 1,
        path.display()
    )]
    NotAnObject { path: PathBuf, index: usize },

    #[error(
        "{object} is defined twice, in {} and in {}.\nNothing was created.",
        first.display(),
        second.display()
    )]
    #[diagnostic(help("Keep one definition, or pass only the files for this preview."))]
    Duplicate {
        object: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("{object} from {} has an invalid Secret value: {error}\nNothing was created.", source_path.display())]
    InvalidSecret {
        object: String,
        source_path: PathBuf,
        error: SecretDecodeError,
    },

    #[error(
        "{object} from {} is a `kubernetes.io/service-account-token` Secret, which a preview \
         cannot copy: the cluster mints a token for the named ServiceAccount into every Secret \
         of that type.\nNothing was created.",
        source_path.display()
    )]
    #[diagnostic(help(
        "Leave that Secret out of the files you pass, or reference the live one from the pod \
         template."
    ))]
    ServiceAccountTokenSecret {
        object: String,
        source_path: PathBuf,
    },

    #[error(
        "{object} from {} has no pod template.\nNothing was created.",
        source_path.display()
    )]
    NoPodTemplate {
        object: String,
        source_path: PathBuf,
    },

    #[error(
        "{object} from {} has no container named `{container}`, the container the preview runs \
         your image in.\nNothing was created.",
        source_path.display()
    )]
    #[diagnostic(help(
        "Name the container in the target (`{target}/container/<name>`) or add it to the \
         manifest."
    ))]
    MissingContainer {
        object: String,
        source_path: PathBuf,
        container: String,
        target: String,
    },

    #[error(
        "The cluster rejected {kind}/{name} from {}:\n{message}\nNothing was created.",
        source_path.display()
    )]
    #[diagnostic(help(
        "The cluster validated the object with a dry run, which creates nothing. Fix the \
         manifest and run the command again, or see what differs with `mirrord preview diff`."
    ))]
    Rejected {
        kind: String,
        name: String,
        source_path: PathBuf,
        message: String,
    },

    #[error("Failed to read {object} from the cluster")]
    #[diagnostic(help(
        "`--resource` compares your manifests with the live objects using your own \
         credentials, so you need `get` on the target and on the ConfigMaps and Secrets it uses."
    ))]
    LiveRead {
        object: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("Failed to validate {object} with the cluster")]
    DryRun {
        object: String,
        #[source]
        source: Box<kube::Error>,
    },
}

/// The role an in-scope object plays in the preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObjectRole {
    Target,
    ConfigMap,
    Secret,
}

/// How an in-scope object compares with the live cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Differs from the live object.
    Changed,
    /// Does not exist in the cluster.
    New,
    /// Same as the live object: nothing happens for it.
    Unchanged,
}

/// One in-scope object and how it compares with the live cluster.
#[derive(Debug)]
pub(crate) struct PlannedObject<'a> {
    pub object: &'a SuppliedObject,
    pub role: ObjectRole,
    pub verdict: Verdict,
    /// Empty exactly when `verdict` is [`Verdict::Unchanged`].
    pub changes: Vec<FieldChange>,
}

/// The outcome of steps 2 to 4: what the preview takes from the files.
#[derive(Debug)]
pub(crate) struct ResourcePlan<'a> {
    /// The paths the user gave, as messages show them (`./k8s/`).
    pub sources: String,
    /// The target, as messages show it (`deployment/app`).
    pub target_display: String,
    /// The target first (when the files define it), then ConfigMaps, then Secrets.
    pub planned: Vec<PlannedObject<'a>>,
    pub out_of_scope: Vec<&'a SuppliedObject>,
    /// Things the user should know that do not stop the command, such as a dry run the user's
    /// credentials do not allow.
    pub notes: Vec<String>,
}

impl ResourcePlan<'_> {
    pub fn target(&self) -> Option<&PlannedObject<'_>> {
        self.planned
            .iter()
            .find(|planned| planned.role == ObjectRole::Target)
    }

    /// The session's `spec.specResources`: the user's pod template when the target changed,
    /// and every changed or new ConfigMap and Secret. Secret values go into `secret_values`,
    /// the contents of the session's secret mounts Secret, never onto the CR. `None` when
    /// nothing differs from the live cluster, which leaves the CR as older CLIs create it.
    pub fn spec_resources(
        &self,
        secret_values: &mut BTreeMap<String, ByteString>,
    ) -> Result<Option<PreviewSpecResources>, ResourcesError> {
        let mut resources = PreviewSpecResources::default();

        for planned in self
            .planned
            .iter()
            .filter(|planned| planned.verdict != Verdict::Unchanged)
        {
            let object = planned.object;
            match planned.role {
                ObjectRole::Target => {
                    let template =
                        scope::pod_template(&object.kind, &object.value).ok_or_else(|| {
                            ResourcesError::NoPodTemplate {
                                object: object.display(),
                                source_path: object.source.clone(),
                            }
                        })?;
                    resources.pod_template = Some(template.to_string());
                }
                ObjectRole::ConfigMap => {
                    let entries = |field: &str| -> BTreeMap<String, String> {
                        compare::comparable_config_map(&object.value)
                            .get(field)
                            .and_then(Value::as_object)
                            .into_iter()
                            .flatten()
                            .filter_map(|(key, value)| {
                                Some((key.clone(), value.as_str()?.to_owned()))
                            })
                            .collect()
                    };
                    resources.config_maps.push(PreviewSpecConfigMap {
                        name: object.name.clone(),
                        data: entries("data"),
                        binary_data: entries("binaryData"),
                    });
                }
                ObjectRole::Secret => {
                    let secret_type = object.value.get("type").and_then(Value::as_str);
                    if secret_type == Some(SERVICE_ACCOUNT_TOKEN_SECRET_TYPE) {
                        return Err(ResourcesError::ServiceAccountTokenSecret {
                            object: object.display(),
                            source_path: object.source.clone(),
                        });
                    }

                    let secret_index = resources.secrets.len();
                    let bytes = compare::secret_bytes(&object.value)
                        .map_err(|error| invalid_secret(object, error))?;

                    let mut keys = BTreeMap::new();
                    for (key_index, (key, value)) in bytes.into_iter().enumerate() {
                        let session_key =
                            PreviewSpecSecret::session_secret_key(secret_index, key_index);
                        secret_values.insert(session_key.clone(), ByteString(value));
                        keys.insert(key, session_key);
                    }

                    resources.secrets.push(PreviewSpecSecret {
                        name: object.name.clone(),
                        r#type: secret_type.map(str::to_owned),
                        keys,
                    });
                }
            }
        }

        Ok((resources != PreviewSpecResources::default()).then_some(resources))
    }
}

/// Secret type the API server fills in itself with a token for the ServiceAccount named in the
/// Secret's annotations. A copy would be a new token for that ServiceAccount, so it is never
/// copied.
const SERVICE_ACCOUNT_TOKEN_SECRET_TYPE: &str = "kubernetes.io/service-account-token";

/// The paths as the user wrote them, for messages.
pub(crate) fn sources_label(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Steps 2 to 4 of the pipeline for `objects`, against the live `target` in `namespace`.
///
/// Reads and dry runs use `client`, the user's own credentials. Nothing is created: every
/// write is a dry run. The first object the cluster rejects ends the plan with
/// [`ResourcesError::Rejected`].
pub(crate) async fn plan<'a>(
    client: &Client,
    objects: &'a [SuppliedObject],
    sources: String,
    target: &KubeResourceTarget,
    namespace: &str,
) -> Result<ResourcePlan<'a>, ResourcesError> {
    let target_ref = TargetRef {
        kind: &target.kind,
        name: &target.name,
        container: &target.container,
        namespace,
    };
    let target_display = target_ref.display();

    let target_api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), namespace, &api_resource(target));
    let live_target = target_api
        .get(&target.name)
        .await
        .map_err(|source| ResourcesError::LiveRead {
            object: target_display.clone(),
            source: Box::new(source),
        })
        .and_then(|object| to_value(&object, &target_display))?;
    let live_template = match scope::workload_ref(&live_target) {
        Some(reference) => live_workload_template(client, namespace, &reference).await?,
        None => scope::pod_template(&target.kind, &live_target),
    }
    .ok_or_else(|| ResourcesError::NoPodTemplate {
        object: target_display.clone(),
        source_path: PathBuf::from("the cluster"),
    })?;

    let scope = scope::select(objects, target_ref, Some(&live_template))?;

    let mut plan = ResourcePlan {
        sources,
        target_display,
        planned: Vec::new(),
        out_of_scope: scope.out_of_scope,
        notes: Vec::new(),
    };

    if let Some(definition) = scope.target {
        let planned = plan_target(
            &target_api,
            definition,
            target_ref,
            &live_template,
            &mut plan.notes,
        )
        .await?;
        plan.planned.push(planned);
    }

    let config_map_api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let dry_run_config_maps: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        namespace,
        &ApiResource::erase::<ConfigMap>(&()),
    );
    for object in scope.config_maps {
        let live = read_live(&config_map_api, object, &mut plan.notes)
            .await?
            .map(|live| compare::comparable_config_map(&live));
        let supplied = compare::comparable_config_map(&object.value);
        let planned = compare_and_validate(
            &dry_run_config_maps,
            object,
            ObjectRole::ConfigMap,
            live,
            &supplied,
            namespace,
            &mut plan.notes,
        )
        .await?;
        plan.planned.push(planned);
    }

    let secret_api: Api<Secret> = Api::namespaced(client.clone(), namespace);
    let dry_run_secrets: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        namespace,
        &ApiResource::erase::<Secret>(&()),
    );
    for object in scope.secrets {
        if let Some(secret_type) = object
            .value
            .get("type")
            .and_then(Value::as_str)
            .filter(|secret_type| UNCOPYABLE_SECRET_TYPES.contains(secret_type))
        {
            return Err(ResourcesError::UnsupportedSecretType {
                object: object.display(),
                source_path: object.source.clone(),
                secret_type: secret_type.to_owned(),
            });
        }
        let supplied = compare::comparable_secret(&object.value)
            .map_err(|error| invalid_secret(object, error))?;
        let live = read_live(&secret_api, object, &mut plan.notes)
            .await?
            .map(|live| {
                compare::comparable_secret(&live).map_err(|error| invalid_secret(object, error))
            })
            .transpose()?;
        let planned = compare_and_validate(
            &dry_run_secrets,
            object,
            ObjectRole::Secret,
            live,
            &supplied,
            namespace,
            &mut plan.notes,
        )
        .await?;
        plan.planned.push(planned);
    }

    Ok(plan)
}

/// Secret types whose copy the API server would refuse or that would not work under another
/// name: a service account token Secret must carry its service account's annotation and is
/// filled in by the token controller.
const UNCOPYABLE_SECRET_TYPES: &[&str] = &["kubernetes.io/service-account-token"];

/// The pod template of a workload a Rollout references, as it is live.
async fn live_workload_template(
    client: &Client,
    namespace: &str,
    reference: &scope::WorkloadRef,
) -> Result<Option<Value>, ResourcesError> {
    let display = format!("{}/{}", reference.kind.to_lowercase(), reference.name);
    let api: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        namespace,
        &api_resource_for(&reference.api_version, &reference.kind),
    );
    let workload = api
        .get(&reference.name)
        .await
        .map_err(|source| ResourcesError::LiveRead {
            object: display.clone(),
            source: Box::new(source),
        })?;
    Ok(scope::pod_template(
        &reference.kind,
        &to_value(&workload, &display)?,
    ))
}

/// Gives an Argo Rollout target defined in the files a pod template of its own when it takes
/// one from another workload (`spec.workloadRef`): the referenced workload's, from the files
/// when they define it (it is then consumed, not reported as out of scope), else the live
/// one. Everything after this step reads the target's template from `spec.template`.
pub(crate) async fn resolve_workload_refs(
    client: &Client,
    objects: &mut Vec<SuppliedObject>,
    target: &KubeResourceTarget,
    namespace: &str,
) -> Result<(), ResourcesError> {
    let in_namespace = |object: &SuppliedObject| {
        object
            .namespace
            .as_deref()
            .is_none_or(|object_namespace| object_namespace == namespace)
    };
    let Some(target_index) = objects.iter().position(|object| {
        object.kind.eq_ignore_ascii_case(&target.kind)
            && object.name == target.name
            && in_namespace(object)
    }) else {
        return Ok(());
    };
    let Some(reference) = objects
        .get(target_index)
        .and_then(|definition| scope::workload_ref(&definition.value))
    else {
        return Ok(());
    };

    let supplied = objects.iter().position(|object| {
        object.kind.eq_ignore_ascii_case(&reference.kind)
            && object.name == reference.name
            && in_namespace(object)
    });
    let template = match supplied {
        Some(index) => {
            let workload = objects.remove(index);
            scope::pod_template(&workload.kind, &workload.value)
        }
        None => live_workload_template(client, namespace, &reference).await?,
    };

    // Removing the referenced workload may have shifted the target's position.
    let Some(definition) = objects.iter_mut().find(|object| {
        object.kind.eq_ignore_ascii_case(&target.kind)
            && object.name == target.name
            && in_namespace(object)
    }) else {
        return Ok(());
    };
    let Some(template) = template else {
        return Err(ResourcesError::NoPodTemplate {
            object: definition.display(),
            source_path: definition.source.clone(),
        });
    };
    if let Some(spec) = definition
        .value
        .get_mut("spec")
        .and_then(Value::as_object_mut)
    {
        spec.remove("workloadRef");
        spec.insert("template".to_owned(), template);
    }

    Ok(())
}

/// Compares the target's template from the files with the live one.
///
/// The API server fills dozens of pod template fields with defaults, so the user's template is
/// first sent through a dry-run create (under a generated name, since the live object already
/// uses the real one) and the defaulted template it returns is what gets compared. The same dry
/// run is the target's validation. When the user may not create the target's kind, the
/// template is compared as written: defaults the live object carries then show up as changes,
/// which only means the preview runs the user's template, the safe direction.
async fn plan_target<'a>(
    api: &Api<DynamicObject>,
    definition: &'a SuppliedObject,
    target: TargetRef<'_>,
    live_template: &Value,
    notes: &mut Vec<String>,
) -> Result<PlannedObject<'a>, ResourcesError> {
    let object = definition.display();
    let supplied_template =
        scope::pod_template(&definition.kind, &definition.value).ok_or_else(|| {
            ResourcesError::NoPodTemplate {
                object: object.clone(),
                source_path: definition.source.clone(),
            }
        })?;

    if !scope::has_container(&supplied_template, target.container) {
        return Err(ResourcesError::MissingContainer {
            object,
            source_path: definition.source.clone(),
            container: target.container.to_owned(),
            target: target.display(),
        });
    }

    let dry_run_object = for_dry_run(&definition.value, target.namespace, true);
    let compared_template = match dry_run_create(api, &dry_run_object, &object).await? {
        DryRun::Accepted(accepted) => {
            scope::pod_template(&definition.kind, &accepted).unwrap_or(supplied_template)
        }
        DryRun::AlreadyExists => supplied_template,
        DryRun::Forbidden => {
            notes.push(forbidden_dry_run_note(definition));
            supplied_template
        }
        DryRun::Rejected(message) => return Err(rejected(definition, message)),
    };

    let changes = compare::diff(
        "spec.template",
        &compare::comparable_template(live_template),
        &compare::comparable_template(&compared_template),
    );
    let verdict = if changes.is_empty() {
        Verdict::Unchanged
    } else {
        Verdict::Changed
    };

    Ok(PlannedObject {
        object: definition,
        role: ObjectRole::Target,
        verdict,
        changes,
    })
}

/// The live counterpart of `object` in the target's namespace, or `None` when there is none.
///
/// A live object the user may not read is treated as missing, with a note: the preview then
/// uses the user's version, and whether it differs cannot be shown.
async fn read_live<K>(
    api: &Api<K>,
    object: &SuppliedObject,
    notes: &mut Vec<String>,
) -> Result<Option<Value>, ResourcesError>
where
    K: kube::Resource + Clone + serde::de::DeserializeOwned + serde::Serialize + std::fmt::Debug,
{
    match api.get_opt(&object.name).await {
        Ok(live) => live
            .map(|live| to_value(&live, &object.display()))
            .transpose(),
        Err(kube::Error::Api(response)) if response.code == 403 => {
            notes.push(format!(
                "Could not read the live {} (your credentials do not allow it); the preview uses \
                 the version from {}.",
                object.display(),
                object.source.display()
            ));
            Ok(None)
        }
        Err(source) => Err(ResourcesError::LiveRead {
            object: object.display(),
            source: Box::new(source),
        }),
    }
}

/// Compares a ConfigMap or Secret with its live version and validates it when it differs.
async fn compare_and_validate<'a>(
    dry_run_api: &Api<DynamicObject>,
    object: &'a SuppliedObject,
    role: ObjectRole,
    live: Option<Value>,
    supplied: &Value,
    namespace: &str,
    notes: &mut Vec<String>,
) -> Result<PlannedObject<'a>, ResourcesError> {
    let (verdict, changes) = match live {
        Some(live) => {
            let changes = compare::diff("", &live, supplied);
            let verdict = if changes.is_empty() {
                Verdict::Unchanged
            } else {
                Verdict::Changed
            };
            (verdict, changes)
        }
        None => (Verdict::New, compare::diff("", &json!({}), supplied)),
    };

    if verdict != Verdict::Unchanged {
        let dry_run_object = for_dry_run(&object.value, namespace, false);
        match dry_run_create(dry_run_api, &dry_run_object, &object.display()).await? {
            DryRun::Accepted(_) | DryRun::AlreadyExists => {}
            DryRun::Forbidden => notes.push(forbidden_dry_run_note(object)),
            DryRun::Rejected(message) => return Err(rejected(object, message)),
        }
    }

    Ok(PlannedObject {
        object,
        role,
        verdict,
        changes,
    })
}

/// What the API server said about a dry-run create.
enum DryRun {
    /// Valid; the object as the API server would have stored it.
    Accepted(Value),
    /// An object with this name exists. Creation validates before it checks for the name, so
    /// this also means the object is valid.
    AlreadyExists,
    /// The user may not create this kind.
    Forbidden,
    /// Invalid, with the API server's explanation.
    Rejected(String),
}

async fn dry_run_create(
    api: &Api<DynamicObject>,
    object: &Value,
    display: &str,
) -> Result<DryRun, ResourcesError> {
    let object: DynamicObject =
        serde_json::from_value(object.clone()).map_err(|error| ResourcesError::DryRun {
            object: display.to_owned(),
            source: Box::new(kube::Error::SerdeError(error)),
        })?;
    let params = PostParams {
        dry_run: true,
        ..Default::default()
    };

    match api.create(&params, &object).await {
        Ok(accepted) => to_value(&accepted, display).map(DryRun::Accepted),
        Err(kube::Error::Api(response)) => match response.code {
            409 => Ok(DryRun::AlreadyExists),
            403 => Ok(DryRun::Forbidden),
            400 | 422 => Ok(DryRun::Rejected(response.message)),
            _ => Err(ResourcesError::DryRun {
                object: display.to_owned(),
                source: Box::new(kube::Error::Api(response)),
            }),
        },
        Err(source) => Err(ResourcesError::DryRun {
            object: display.to_owned(),
            source: Box::new(source),
        }),
    }
}

/// `object` as a dry-run create request: placed in the target's namespace, without what the
/// API server writes itself (a manifest saved with `kubectl get -o yaml` carries
/// `resourceVersion`, which a create refuses), and with a generated name when
/// `generate_name` is set.
fn for_dry_run(object: &Value, namespace: &str, generate_name: bool) -> Value {
    let mut object = object.clone();
    let Some(fields) = object.as_object_mut() else {
        return object;
    };
    fields.remove("status");

    let metadata = fields
        .entry("metadata")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(metadata) = metadata.as_object_mut() {
        for server_field in [
            "resourceVersion",
            "uid",
            "creationTimestamp",
            "managedFields",
            "generation",
            "selfLink",
            "ownerReferences",
        ] {
            metadata.remove(server_field);
        }
        metadata.insert("namespace".to_owned(), Value::String(namespace.to_owned()));

        if generate_name && let Some(Value::String(name)) = metadata.remove("name") {
            metadata.insert("generateName".to_owned(), Value::String(format!("{name}-")));
        }
    }

    object
}

fn api_resource(target: &KubeResourceTarget) -> ApiResource {
    api_resource_for(&target.api_version, &target.kind)
}

fn api_resource_for(api_version: &str, kind: &str) -> ApiResource {
    let (group, version) = api_version.rsplit_once('/').unwrap_or(("", api_version));
    ApiResource::from_gvk(&GroupVersionKind::gvk(group, version, kind))
}

fn to_value<T: serde::Serialize>(object: &T, display: &str) -> Result<Value, ResourcesError> {
    serde_json::to_value(object).map_err(|error| ResourcesError::DryRun {
        object: display.to_owned(),
        source: Box::new(kube::Error::SerdeError(error)),
    })
}

fn rejected(object: &SuppliedObject, message: String) -> ResourcesError {
    ResourcesError::Rejected {
        kind: object.kind.clone(),
        name: object.name.clone(),
        source_path: object.source.clone(),
        message,
    }
}

fn invalid_secret(object: &SuppliedObject, error: SecretDecodeError) -> ResourcesError {
    ResourcesError::InvalidSecret {
        object: object.display(),
        source_path: object.source.clone(),
        error,
    }
}

fn forbidden_dry_run_note(object: &SuppliedObject) -> String {
    format!(
        "Could not validate {} from {} with the cluster (your credentials do not allow a dry-run \
         create of it); the operator reports it if the cluster rejects it.",
        object.display(),
        object.source.display()
    )
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // Tests read JSON fixtures with `value[key]`; a panic just fails the test.
mod tests {
    use base64::prelude::*;

    use super::*;

    fn object(value: Value) -> SuppliedObject {
        SuppliedObject::new(Path::new("./k8s/all.yaml"), 0, value).unwrap()
    }

    #[test]
    fn dry_run_request_drops_server_fields_and_can_generate_a_name() {
        let saved = json!({
            "kind": "Deployment",
            "metadata": {
                "name": "app",
                "namespace": "other",
                "resourceVersion": "42",
                "uid": "u",
                "managedFields": [],
                "labels": {"app": "app"},
            },
            "spec": {},
            "status": {"replicas": 1},
        });

        assert_eq!(
            for_dry_run(&saved, "default", true),
            json!({
                "kind": "Deployment",
                "metadata": {
                    "generateName": "app-",
                    "namespace": "default",
                    "labels": {"app": "app"},
                },
                "spec": {},
            })
        );
        assert_eq!(
            for_dry_run(&saved, "default", false)["metadata"]["name"],
            json!("app")
        );
    }

    #[test]
    fn api_resource_handles_core_and_grouped_kinds() {
        let target = |api_version: &str, kind: &str| KubeResourceTarget {
            api_version: api_version.to_owned(),
            kind: kind.to_owned(),
            name: "app".to_owned(),
            container: "app".to_owned(),
        };

        let deployment = api_resource(&target("apps/v1", "Deployment"));
        assert_eq!(
            (deployment.group.as_str(), deployment.plural.as_str()),
            ("apps", "deployments")
        );
        let pod = api_resource(&target("v1", "Pod"));
        assert_eq!((pod.group.as_str(), pod.plural.as_str()), ("", "pods"));
    }

    /// A service-account-token Secret is filled in by the API server for the ServiceAccount its
    /// annotation names, so copying it would mint a token. It is refused before anything is
    /// created rather than left for the cluster to reject the copy without that annotation.
    #[test]
    fn spec_resources_refuse_a_service_account_token_secret() {
        let secret = object(json!({
            "kind": "Secret",
            "metadata": {
                "name": "app-token",
                "annotations": {"kubernetes.io/service-account.name": "app"},
            },
            "type": "kubernetes.io/service-account-token",
        }));
        let plan = ResourcePlan {
            sources: "./k8s/".to_owned(),
            target_display: "deployment/app".to_owned(),
            planned: vec![PlannedObject {
                object: &secret,
                role: ObjectRole::Secret,
                verdict: Verdict::New,
                changes: vec![],
            }],
            out_of_scope: vec![],
            notes: vec![],
        };

        let mut secret_values = BTreeMap::new();
        let error = plan.spec_resources(&mut secret_values).unwrap_err();

        assert!(
            matches!(error, ResourcesError::ServiceAccountTokenSecret { ref object, .. } if object == "secret/app-token"),
            "{error:?}"
        );
        assert!(secret_values.is_empty());
    }

    /// Only what differs travels on the CR, and Secret values go to the session Secret instead,
    /// under keys the CR names.
    #[test]
    fn spec_resources_carry_changed_objects_and_keep_secret_values_off_the_cr() {
        let deployment = object(json!({
            "kind": "Deployment",
            "metadata": {"name": "app"},
            "spec": {"template": {"spec": {"containers": [{"name": "app"}]}}},
        }));
        let changed_map = object(json!({
            "kind": "ConfigMap",
            "metadata": {"name": "app-config"},
            "data": {"PORT": 8080},
        }));
        let unchanged_map = object(json!({
            "kind": "ConfigMap",
            "metadata": {"name": "same"},
            "data": {"A": "1"},
        }));
        let secret = object(json!({
            "kind": "Secret",
            "metadata": {"name": "creds"},
            "type": "Opaque",
            "data": {"user": BASE64_STANDARD.encode("admin")},
            "stringData": {"password": "hunter2"},
        }));

        let planned = |object, role, verdict| PlannedObject {
            object,
            role,
            verdict,
            changes: vec![],
        };
        let plan = ResourcePlan {
            sources: "./k8s/".to_owned(),
            target_display: "deployment/app".to_owned(),
            planned: vec![
                planned(&deployment, ObjectRole::Target, Verdict::Changed),
                planned(&changed_map, ObjectRole::ConfigMap, Verdict::Changed),
                planned(&unchanged_map, ObjectRole::ConfigMap, Verdict::Unchanged),
                planned(&secret, ObjectRole::Secret, Verdict::New),
            ],
            out_of_scope: vec![],
            notes: vec![],
        };

        let mut secret_values = BTreeMap::new();
        let resources = plan.spec_resources(&mut secret_values).unwrap().unwrap();

        assert_eq!(
            resources,
            PreviewSpecResources {
                pod_template: Some(r#"{"spec":{"containers":[{"name":"app"}]}}"#.to_owned()),
                config_maps: vec![PreviewSpecConfigMap {
                    name: "app-config".to_owned(),
                    data: BTreeMap::from([("PORT".to_owned(), "8080".to_owned())]),
                    binary_data: BTreeMap::new(),
                }],
                secrets: vec![PreviewSpecSecret {
                    name: "creds".to_owned(),
                    r#type: Some("Opaque".to_owned()),
                    keys: BTreeMap::from([
                        ("password".to_owned(), "s0-0".to_owned()),
                        ("user".to_owned(), "s0-1".to_owned()),
                    ]),
                }],
            }
        );
        assert_eq!(
            secret_values,
            BTreeMap::from([
                ("s0-0".to_owned(), ByteString(b"hunter2".to_vec())),
                ("s0-1".to_owned(), ByteString(b"admin".to_vec())),
            ])
        );
        assert!(
            !serde_json::to_string(&resources)
                .unwrap()
                .contains("hunter2")
        );
    }

    /// Nothing that differs from the live cluster means no `specResources` at all, so the CR
    /// is exactly what an older CLI creates.
    #[test]
    fn nothing_changed_sends_no_spec_resources() {
        let deployment = object(json!({
            "kind": "Deployment",
            "metadata": {"name": "app"},
            "spec": {"template": {"spec": {"containers": [{"name": "app"}]}}},
        }));
        let plan = ResourcePlan {
            sources: "./k8s/".to_owned(),
            target_display: "deployment/app".to_owned(),
            planned: vec![PlannedObject {
                object: &deployment,
                role: ObjectRole::Target,
                verdict: Verdict::Unchanged,
                changes: vec![],
            }],
            out_of_scope: vec![],
            notes: vec![],
        };

        let mut secret_values = BTreeMap::new();
        assert_eq!(plan.spec_resources(&mut secret_values).unwrap(), None);
        assert!(secret_values.is_empty());
    }

    #[test]
    fn rejection_message_matches_the_documented_format() {
        let config_map = object(json!({
            "kind": "ConfigMap",
            "metadata": {"name": "qa_workflowsvc"},
        }));
        let config_map = SuppliedObject {
            source: PathBuf::from("./k8s/configmap.yaml"),
            ..config_map
        };

        let error = rejected(
            &config_map,
            "metadata.name: Invalid value: \"qa_workflowsvc\": a lowercase RFC 1123 subdomain \
             must consist of lower case alphanumeric characters, '-' or '.'"
                .to_owned(),
        );

        assert_eq!(
            error.to_string(),
            "The cluster rejected ConfigMap/qa_workflowsvc from ./k8s/configmap.yaml:\n\
             metadata.name: Invalid value: \"qa_workflowsvc\": a lowercase RFC 1123 subdomain \
             must consist of lower case alphanumeric characters, '-' or '.'\n\
             Nothing was created."
        );
    }
}
