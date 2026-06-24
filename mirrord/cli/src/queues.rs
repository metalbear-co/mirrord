use kube::{Api, Resource};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::crd::queue_split::{QueueSplitCrd, QueueSplitQueue};
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
/// the header and every data row are built from the same list, so they can never
/// drift out of order.
#[derive(Display, EnumIter, Clone, Copy)]
enum Column {
    #[strum(serialize = "SESSION")]
    Session,
    #[strum(serialize = "NAMESPACE")]
    Namespace,
    #[strum(serialize = "TARGET")]
    Target,
    #[strum(serialize = "PHASE")]
    Phase,
    #[strum(serialize = "READY")]
    Ready,
    #[strum(serialize = "QUEUES")]
    Queues,
    #[strum(serialize = "TARGET PODS")]
    TargetPods,
}

impl Column {
    /// The cell value for this column for a given queue split.
    fn value(self, split: &QueueSplitCrd) -> String {
        let spec = &split.spec;
        let status = split.status.as_ref();
        let phase = status.map(|s| s.phase.as_str()).unwrap_or("-");
        match self {
            Self::Session => spec.session.clone(),
            Self::Namespace => split.meta().namespace.clone().unwrap_or_default(),
            Self::Target => format!("{}/{}", spec.target.kind, spec.target.name),
            Self::Phase => phase.to_owned(),
            Self::Ready => if phase == "Ready" { "yes" } else { "no" }.to_owned(),
            Self::Queues => join_or_dash(
                status
                    .into_iter()
                    .flat_map(|s| s.queues.iter())
                    .map(render_queue),
            ),
            Self::TargetPods => join_or_dash(
                status
                    .into_iter()
                    .flat_map(|s| s.target_pods.iter())
                    .map(|pod| {
                        format!("{} (patched={}, ready={})", pod.name, pod.patched, pod.ready)
                    }),
            ),
        }
    }
}

/// One line per resolved queue: the concrete queue/topic/subscription name, with
/// the Kafka consumer group appended when present.
fn render_queue(queue: &QueueSplitQueue) -> String {
    let name = queue
        .queue
        .as_ref()
        .or(queue.topic.as_ref())
        .or(queue.subscription.as_ref())
        .cloned()
        .unwrap_or_else(|| queue.id.clone());
    match &queue.consumer_group {
        Some(group) => format!("{name} (group={group})"),
        None => name,
    }
}

/// Joins lines with newlines, or returns `-` when there is nothing to show.
fn join_or_dash(values: impl Iterator<Item = String>) -> String {
    let joined = values.collect::<Vec<_>>().join("\n");
    if joined.is_empty() {
        "-".to_owned()
    } else {
        joined
    }
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
