use std::time::Duration;

use k8s_openapi::jiff::Timestamp;
use kube::{Api, Resource};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::crd::{
    queue_split::{QueueSplit, QueueSplitFilter, QueueSplitQueue, QueueSplitTargetPod},
    session::SessionTarget,
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{Cell, Row, Table, format};
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
    fn value(self, split: &QueueSplit) -> String {
        let spec = &split.spec;
        let status = split.status.as_ref();
        let phase = status.map(|s| s.phase.as_str()).unwrap_or("-");
        match self {
            Self::Session => spec.session.clone(),
            Self::User => spec.owner.to_string(),
            Self::Namespace => split.meta().namespace.clone().unwrap_or_default(),
            Self::Target => format!("{}/{}", spec.target.kind, spec.target.name),
            Self::Phase => phase.to_owned(),
            Self::Duration => render_duration(split),
        }
    }
}

/// How long the session has been running, derived from when the view object was
/// created. Falls back to `-` if the timestamp is missing.
fn render_duration(split: &QueueSplit) -> String {
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
    match &args.command {
        QueuesCommand::Status { .. } => status_command(args).await,
    }
}

async fn status_command(args: QueuesArgs) -> CliResult<()> {
    let QueuesCommand::Status { name } = &args.command;
    let name = name.clone();

    let mut progress = ProgressTracker::from_env("Queue Splitting Status");
    let mut fetch_progress = progress.subtask("fetching queue splits");

    let mut cfg_context =
        ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file);
    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    let client = kube_client_from_layer_config(&layer_config).await?;

    // The queue-splitting status view is built on the fly by the operator. We
    // list it across all namespaces so one command shows every active session,
    // including ones the primary aggregates from other clusters.
    let api: Api<QueueSplit> = Api::all(client);
    let splits = list_resource_if_defined(&api, &mut fetch_progress)
        .await?
        .unwrap_or_default();
    fetch_progress.success(None);

    let Some(name) = name else {
        return print_table(progress, &splits);
    };

    // A split name encodes the session and target, so it is unique across all
    // namespaces; matching by name lets the user ask for one without knowing
    // which namespace (or cluster) it lives in.
    let Some(split) = splits
        .iter()
        .find(|split| split.meta().name.as_deref() == Some(&name))
    else {
        progress.failure(Some(&format!("No queue-splitting session named '{name}'")));
        return Ok(());
    };

    progress.success(None);
    print_detail(split);

    Ok(())
}

/// Prints the one-row-per-session summary table used when no name is given.
fn print_table(mut progress: ProgressTracker, splits: &[QueueSplit]) -> CliResult<()> {
    if splits.is_empty() {
        progress.success(Some("No active queue-splitting sessions found"));
        return Ok(());
    }

    let mut table = Table::new();
    table.add_row(Row::new(
        Column::iter().map(|c| Cell::new(&c.to_string())).collect(),
    ));

    for split in splits {
        table.add_row(Row::new(
            Column::iter().map(|c| Cell::new(&c.value(split))).collect(),
        ));
    }

    progress.success(None);
    table.printstd();

    Ok(())
}

/// Fields of the single-split summary block, in print order. The variant name
/// is the label (via `Display`), and `value` returns `None` for fields that are
/// absent (like `Message`) so they are skipped. Adding a line is one variant
/// plus one match arm.
#[derive(Display, EnumIter, Clone, Copy)]
enum DetailField {
    Session,
    Namespace,
    User,
    Target,
    Phase,
    Duration,
    Message,
}

impl DetailField {
    fn value(self, split: &QueueSplit) -> Option<String> {
        let spec = &split.spec;
        let status = split.status.as_ref();
        match self {
            Self::Session => Some(spec.session.clone()),
            Self::Namespace => Some(split.meta().namespace.clone().unwrap_or_else(dash)),
            Self::User => Some(split.spec.owner.to_string()),
            Self::Target => Some(render_target(&spec.target)),
            Self::Phase => Some(status.map(|s| s.phase.clone()).unwrap_or_else(dash)),
            Self::Duration => Some(render_duration(split)),
            Self::Message => status.and_then(|s| s.message.clone()),
        }
    }
}

/// Broker-specific fields of a requested filter, shown together in the table's
/// `DETAILS` cell as `key=value`. `id` and `type` are their own columns, so
/// only the parts that differ per broker live here. Adding one is one variant
/// plus one match arm.
#[derive(Display, EnumIter, Clone, Copy)]
#[strum(serialize_all = "lowercase")]
enum FilterDetail {
    Filter,
    Jq,
}

impl FilterDetail {
    fn value(self, filter: &QueueSplitFilter) -> Option<String> {
        match self {
            Self::Filter => (!filter.message_filter.is_empty()).then(|| {
                let joined = filter
                    .message_filter
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("{{{joined}}}")
            }),
            Self::Jq => filter.jq_filter.clone(),
        }
    }
}

/// Broker-specific fields of a resolved queue, shown together in the table's
/// `DETAILS` cell. Each broker fills only the ones it has (SQS has `queue`,
/// Kafka has `topic`/`group`), so there are no empty columns.
#[derive(Display, EnumIter, Clone, Copy)]
#[strum(serialize_all = "lowercase")]
enum QueueDetail {
    Queue,
    Topic,
    Group,
    Subscription,
}

impl QueueDetail {
    fn value(self, queue: &QueueSplitQueue) -> Option<String> {
        match self {
            Self::Queue => queue.queue.clone(),
            Self::Topic => queue.topic.clone(),
            Self::Group => queue.consumer_group.clone(),
            Self::Subscription => queue.subscription.clone(),
        }
    }
}

/// Columns of the target-pods table.
#[derive(Display, EnumIter, Clone, Copy)]
#[strum(serialize_all = "UPPERCASE")]
enum PodField {
    Name,
    Patched,
    Ready,
}

impl PodField {
    fn value(self, pod: &QueueSplitTargetPod) -> Option<String> {
        match self {
            Self::Name => Some(pod.name.clone()),
            Self::Patched => Some(pod.patched.to_string()),
            Self::Ready => Some(pod.ready.to_string()),
        }
    }
}

/// Prints the full detail of a single split: a key/value summary, then a table
/// each for the requested filters, the resolved queues, and the target pods.
fn print_detail(split: &QueueSplit) {
    let status = split.status.as_ref();

    println!(
        "Queue Split: {}",
        split.meta().name.as_deref().unwrap_or("-")
    );

    // The summary is one record, so it reads best as borderless label/value
    // rows rather than a wide single-row table.
    let mut summary = Table::new();
    summary.set_format(*format::consts::FORMAT_CLEAN);
    for field in DetailField::iter() {
        if let Some(value) = field.value(split) {
            summary.add_row(Row::new(vec![
                Cell::new(&field.to_string()),
                Cell::new(&value),
            ]));
        }
    }
    summary.printstd();

    print_broker_table("Filters", &split.spec.filters, |filter| {
        (
            &filter.id,
            &filter.queue_type,
            detail_cell::<FilterDetail>(|d| d.value(filter)),
        )
    });
    print_broker_table(
        "Queues",
        status.map(|s| s.queues.as_slice()).unwrap_or_default(),
        |queue| {
            (
                &queue.id,
                &queue.queue_type,
                detail_cell::<QueueDetail>(|d| d.value(queue)),
            )
        },
    );
    print_field_table::<PodField, _>(
        "Target pods",
        status.map(|s| s.target_pods.as_slice()).unwrap_or_default(),
        PodField::value,
    );
}

/// Prints a titled `ID | TYPE | DETAILS` table. Every broker shares the `ID` and
/// `TYPE` columns; `row` returns those plus a ready-made `DETAILS` string of the
/// broker-specific fields, so a session mixing brokers has no empty columns. An
/// empty list prints `(none)` instead of a bare header.
fn print_broker_table<T>(title: &str, items: &[T], row: impl Fn(&T) -> (&str, &str, String)) {
    println!("\n{title}:");
    if items.is_empty() {
        println!("(none)");
        return;
    }

    let mut table = Table::new();
    table.add_row(Row::new(vec![
        Cell::new("ID"),
        Cell::new("TYPE"),
        Cell::new("DETAILS"),
    ]));
    for item in items {
        let (id, kind, details) = row(item);
        table.add_row(Row::new(vec![
            Cell::new(id),
            Cell::new(kind),
            Cell::new(&details),
        ]));
    }
    table.printstd();
}

/// Joins a detail enum's fields into one `key=value ...` cell, skipping any
/// field that is `None`. Shared by the filter and queue `DETAILS` columns.
fn detail_cell<F>(value: impl Fn(F) -> Option<String>) -> String
where
    F: IntoEnumIterator + std::fmt::Display + Copy,
{
    F::iter()
        .filter_map(|field| value(field).map(|value| format!("{field}={value}")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Prints a titled table whose columns are the field enum's variants and whose
/// rows are the items. Used for fixed-shape lists like target pods, where every
/// column always has a value. An empty list prints `(none)`.
fn print_field_table<F, T>(title: &str, items: &[T], cell: impl Fn(F, &T) -> Option<String>)
where
    F: IntoEnumIterator + std::fmt::Display + Copy,
{
    println!("\n{title}:");
    if items.is_empty() {
        println!("(none)");
        return;
    }

    let mut table = Table::new();
    table.add_row(Row::new(
        F::iter()
            .map(|field| Cell::new(&field.to_string()))
            .collect(),
    ));
    for item in items {
        table.add_row(Row::new(
            F::iter()
                .map(|field| Cell::new(&cell(field, item).unwrap_or_default()))
                .collect(),
        ));
    }
    table.printstd();
}

fn render_target(target: &SessionTarget) -> String {
    let mut line = format!("{}/{}", target.kind, target.name);
    if !target.container.is_empty() {
        line.push_str(&format!(" (container: {})", target.container));
    }
    line
}

/// Placeholder used when an optional summary value is missing.
fn dash() -> String {
    "-".to_owned()
}
