use std::time::Duration;

use k8s_openapi::jiff::Timestamp;
use kube::{Api, Resource};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::crd::queue_split::QueueSplitCrd;
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{Cell, Row, Table};
use strum::IntoEnumIterator;
use strum_macros::{Display, EnumIter};

use crate::{
    CliResult,
    config::{QueuesArgs, QueuesCommand},
    kube::{kube_client_from_layer_config, list_resource_if_defined},
};

/// Columns of the `mirrord queues` status table. Keeping them in one enum means
/// the header and every data row are built from the same list in same order.
#[derive(Display, EnumIter, Clone, Copy)]
enum Column {
    #[strum(serialize = "SESSION")]
    Session,
    #[strum(serialize = "USER")]
    User,
    #[strum(serialize = "NAMESPACE")]
    Namespace,
    #[strum(serialize = "TARGET")]
    Target,
    #[strum(serialize = "PHASE")]
    Phase,
    #[strum(serialize = "DURATION")]
    Duration,
}

impl Column {
    /// The cell value for this column for a given queue split.
    fn value(self, split: &QueueSplitCrd) -> String {
        let spec = &split.spec;
        let status = split.status.as_ref();
        let phase = status.map(|s| s.phase.as_str()).unwrap_or("-");
        match self {
            Self::Session => spec.session.clone(),
            Self::User => render_user(split),
            Self::Namespace => split.meta().namespace.clone().unwrap_or_default(),
            Self::Target => format!("{}/{}", spec.target.kind, spec.target.name),
            Self::Phase => phase.to_owned(),
            Self::Duration => render_duration(split),
        }
    }
}

/// The developer who started the session, in the same `user/k8s-user@host`
/// shape that `mirrord operator status` prints, so both views read the same.
fn render_user(split: &QueueSplitCrd) -> String {
    let owner = &split.spec.owner;
    format!(
        "{}/{}@{}",
        owner.username, owner.k8s_username, owner.hostname
    )
}

/// How long the session has been running, derived from when the view object was
/// created. Falls back to `-` if the timestamp is missing.
fn render_duration(split: &QueueSplitCrd) -> String {
    split
        .meta()
        .creation_timestamp
        .as_ref()
        .map(|created| {
            let secs = Timestamp::now().duration_since(created.0).as_secs().max(0) as u64;
            humantime::format_duration(Duration::from_secs(secs)).to_string()
        })
        .unwrap_or_else(|| "-".to_owned())
}

pub(crate) async fn queues_command(args: QueuesArgs) -> CliResult<()> {
    match args.command {
        QueuesCommand::Status => status_command(args).await,
    }
}

async fn status_command(args: QueuesArgs) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("Queue Splitting Status");
    let mut fetch_progress = progress.subtask("fetching queue splits");

    let mut cfg_context =
        ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file);
    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    let client = kube_client_from_layer_config(&layer_config).await?;

    // The queue-splitting status view is built on the fly by the operator. We
    // list it across all namespaces so one command shows every active session,
    // including ones the primary aggregates from other clusters.
    let api: Api<QueueSplitCrd> = Api::all(client);
    let splits = list_resource_if_defined(&api, &mut fetch_progress)
        .await?
        .unwrap_or_default();
    fetch_progress.success(None);

    if splits.is_empty() {
        progress.success(Some("No active queue-splitting sessions found"));
        return Ok(());
    }

    let mut table = Table::new();
    table.add_row(Row::new(
        Column::iter().map(|c| Cell::new(&c.to_string())).collect(),
    ));

    for split in &splits {
        table.add_row(Row::new(
            Column::iter().map(|c| Cell::new(&c.value(split))).collect(),
        ));
    }

    progress.success(None);
    table.printstd();

    Ok(())
}
