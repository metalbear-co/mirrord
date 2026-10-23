//! The Resources view: every mirrord resource type the cluster serves, and the objects of any of
//! them, with no code per type.
//!
//! The types are discovered from the API server (every group under `metalbear.co`), and their
//! objects are listed in the API server's own table format, the one `kubectl get` prints, so the
//! columns are whatever the type's definition declares. A type the operator adds later shows up
//! here, and on the `:` command line, without a change to the TUI.

use std::{
    ops::ControlFlow,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyModifiers};
use futures_util::future::join_all;
use k8s_openapi::jiff::Timestamp;
use kube::{
    Api, Client, Resource,
    api::{ApiResource, DynamicObject, ListParams},
    discovery::{self, Scope as DiscoveryScope, verbs},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
};
use serde_json::Value;
use tokio::{
    sync::{Notify, watch},
    time::MissedTickBehavior,
};

use crate::{
    context::Context,
    kit::{
        confirm::{Answer, Confirm},
        describe::{Describe, DescribeOutcome},
        hints::Hint,
        panel,
        search::{SearchBox, SearchOutcome},
        session_scope::{self, SessionScope},
        state_view::{self, EmptyText, Fetch, Listing},
        table::DataTable,
    },
    screens::Screen,
    theme,
};

/// How often the objects of the open type are listed again.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// How long the outcome of a delete stays on the panel.
const FLASH_FOR: Duration = Duration::from_secs(6);

/// API groups whose types the view lists: the operator's and every group of its CRDs.
const GROUP_SUFFIX: &str = "metalbear.co";

/// The `Accept` header that asks for the table format, falling back to plain JSON for servers
/// that do not offer it.
const TABLE_ACCEPT: &str = "application/json;as=Table;v=v1;g=meta.k8s.io,application/json";

/// A resource type the cluster serves.
#[derive(Clone, Debug)]
pub struct Kind {
    pub resource: ApiResource,
    pub namespaced: bool,
}

impl Kind {
    /// What `:` takes for it: the plural, such as `mirrordsplitconfigs`.
    pub fn plural(&self) -> &str {
        &self.resource.plural
    }
}

/// One listed object.
#[derive(Clone, Debug)]
struct Item {
    name: String,
    namespace: Option<String>,
    cells: Vec<String>,
    /// The object itself, for telling whether it belongs to a session running here.
    object: Value,
}

/// The objects of the open type, with the columns to show them in.
#[derive(Default)]
struct Objects {
    columns: Vec<String>,
    listing: Listing<Item>,
}

pub struct ResourcesScreen {
    context: Context,
    kinds: Arc<RwLock<Fetch<Vec<Kind>>>>,
    /// API groups that did not answer discovery, named on the panel.
    unavailable: Arc<RwLock<Vec<String>>>,
    /// The type being listed; `None` lists the types themselves.
    open: watch::Sender<Option<Kind>>,
    objects: Arc<RwLock<Objects>>,
    table: DataTable,
    search: SearchBox,
    /// "Only my sessions" (`m`).
    scope: SessionScope,
    describe: Option<Describe>,
    /// A describe on its way from the API server.
    describing: Arc<RwLock<Option<Describe>>>,
    confirm: Option<((String, Option<String>), Confirm)>,
    flash: Arc<RwLock<Option<(String, Style, Instant)>>>,
    refresh: Arc<Notify>,
}

impl ResourcesScreen {
    /// The types found so far, in the order `:` offers them.
    pub fn kinds(&self) -> Vec<Kind> {
        match self.kinds.read().as_deref() {
            Ok(Fetch::Ready(kinds)) => kinds.clone(),
            _ => Vec::new(),
        }
    }

    /// Opens the type at `index` of [`ResourcesScreen::kinds`].
    pub fn open(&mut self, index: usize) {
        let kind = self.kinds().into_iter().nth(index);
        self.show(kind);
    }

    fn show(&mut self, kind: Option<Kind>) {
        if let Ok(mut objects) = self.objects.write() {
            *objects = Objects::default();
        }
        self.table.select_first();
        self.search = SearchBox::default();
        self.open.send_replace(kind);
        self.refresh.notify_one();
    }

    /// Finds the types under `metalbear.co` again whenever the connection changes.
    async fn discover(
        mut context: Context,
        kinds: Arc<RwLock<Fetch<Vec<Kind>>>>,
        unavailable: Arc<RwLock<Vec<String>>>,
    ) {
        loop {
            let client = connected(&context.client.borrow_and_update());
            let (state, down) = match client {
                None => (Fetch::Disconnected, Vec::new()),
                Some(client) => match discover_kinds(&client).await {
                    Ok((found, down)) => (Fetch::Ready(found), down),
                    Err(error) => (
                        Fetch::Failed(state_view::explain(&error, "API groups")),
                        Vec::new(),
                    ),
                },
            };
            if let Ok(mut slot) = kinds.write() {
                *slot = state;
            }
            if let Ok(mut slot) = unavailable.write() {
                *slot = down;
            }
            context.request_redraw();
            if context.client.changed().await.is_err() {
                break;
            }
        }
    }

    /// Lists the objects of the open type, again on every change of type, scope or connection,
    /// on request, and every few seconds.
    async fn list(
        mut context: Context,
        mut open: watch::Receiver<Option<Kind>>,
        objects: Arc<RwLock<Objects>>,
        refresh: Arc<Notify>,
    ) {
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                changed = open.changed() => if changed.is_err() { break },
                changed = context.client.changed() => if changed.is_err() { break },
                changed = context.scope.changed() => if changed.is_err() { break },
                _ = refresh.notified() => {},
                _ = interval.tick() => {},
            }

            let Some(kind) = open.borrow_and_update().clone() else {
                continue;
            };
            let namespace = context.scope.borrow_and_update().namespace.clone();
            let client = connected(&context.client.borrow_and_update());
            let (columns, state) = match client {
                None => (Vec::new(), Fetch::Disconnected),
                Some(client) => match fetch_table(&client, &kind, namespace.as_deref()).await {
                    Ok((columns, items)) => (columns, Fetch::Ready(items)),
                    Err(error) => (
                        Vec::new(),
                        Fetch::Failed(state_view::explain(&error, kind.plural())),
                    ),
                },
            };

            if let Ok(mut slot) = objects.write() {
                slot.columns = columns;
                slot.listing.replace(state);
            }
            context.request_redraw();
        }
    }

    fn matching_items<'a>(&self, items: &'a [Item]) -> Vec<&'a Item> {
        let filter = self.scope.current();
        items
            .iter()
            .filter(|item| {
                session_scope::keeps(&filter, &item.object)
                    && self.search.matches_any(item.cells.iter())
            })
            .collect()
    }

    fn matching_kinds<'a>(&self, kinds: &'a [Kind]) -> Vec<&'a Kind> {
        kinds
            .iter()
            .filter(|kind| {
                self.search.matches_any([
                    kind.plural(),
                    kind.resource.kind.as_str(),
                    kind.resource.group.as_str(),
                ])
            })
            .collect()
    }

    /// The object under the cursor, by name and namespace.
    fn selected_item(&self) -> Option<(String, Option<String>)> {
        let objects = self.objects.read().ok()?;
        let Fetch::Ready(items) = &objects.listing.state else {
            return None;
        };
        let index = self.table.selected()?;
        self.matching_items(items)
            .get(index)
            .map(|item| (item.name.clone(), item.namespace.clone()))
    }

    fn api(&self, kind: &Kind, namespace: Option<&str>) -> Option<Api<DynamicObject>> {
        let client = connected(&self.context.client.borrow())?;
        Some(match (kind.namespaced, namespace) {
            (true, Some(namespace)) => Api::namespaced_with(client, namespace, &kind.resource),
            _ => Api::all_with(client, &kind.resource),
        })
    }

    /// Fetches the object under the cursor whole and shows it as YAML once it arrives.
    fn describe_selected(&self) {
        let Some(kind) = self.open.borrow().clone() else {
            return;
        };
        let Some((name, namespace)) = self.selected_item() else {
            return;
        };
        let Some(api) = self.api(&kind, namespace.as_deref()) else {
            return;
        };
        let slot = self.describing.clone();
        let flash = self.flash.clone();
        let redraw = self.context.redraw.clone();
        tokio::spawn(async move {
            match api.get(&name).await {
                Ok(object) => {
                    if let Ok(mut slot) = slot.write() {
                        *slot = Some(Describe::new(
                            format!("{} {name}", kind.resource.kind),
                            &object,
                        ));
                    }
                }
                Err(error) => set_flash(
                    &flash,
                    format!("could not read {name}: {error}"),
                    theme::error(),
                ),
            }
            redraw.notify_one();
        });
    }

    fn delete(&self, name: String, namespace: Option<String>) {
        let Some(kind) = self.open.borrow().clone() else {
            return;
        };
        let Some(api) = self.api(&kind, namespace.as_deref()) else {
            return;
        };
        let flash = self.flash.clone();
        let refresh = self.refresh.clone();
        tokio::spawn(async move {
            match api.delete(&name, &Default::default()).await {
                Ok(_) => set_flash(
                    &flash,
                    format!("deleted {name}"),
                    Style::default().fg(theme::MINT),
                ),
                Err(error) => set_flash(
                    &flash,
                    format!("could not delete {name}: {error}"),
                    theme::error(),
                ),
            }
            refresh.notify_one();
        });
    }

    fn current_flash(&self) -> Option<(String, Style)> {
        self.flash.read().ok().and_then(|flash| {
            flash
                .as_ref()
                .filter(|(_, _, until)| Instant::now() < *until)
                .map(|(message, style, _)| (message.clone(), *style))
        })
    }

    fn draw_kinds(&mut self, frame: &mut Frame, area: Rect) {
        let kinds = self.kinds.read().map(|kinds| match &*kinds {
            Fetch::Ready(kinds) => Fetch::Ready(kinds.clone()),
            Fetch::Failed(error) => Fetch::Failed(error.clone()),
            Fetch::Disconnected => Fetch::Disconnected,
            Fetch::Loading => Fetch::Loading,
        });
        let kinds = kinds.unwrap_or_default();
        let matching = match &kinds {
            Fetch::Ready(kinds) => self.matching_kinds(kinds).into_iter().cloned().collect(),
            _ => Vec::new(),
        };
        let total = match &kinds {
            Fetch::Ready(kinds) => Some(kinds.len()),
            _ => None,
        };
        let count = total.map(|total| {
            if matching.len() < total {
                format!("{}/{total}", matching.len())
            } else {
                total.to_string()
            }
        });

        let down = self
            .unavailable
            .read()
            .map(|down| down.join(", "))
            .unwrap_or_default();
        let message = (!down.is_empty()).then(|| format!("not answering: {down}"));
        let block = panel::block(
            "resources",
            count.as_deref(),
            Some("mirrord resource types"),
            message
                .as_deref()
                .map(|message| (message, theme::warning())),
        );
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let empty = EmptyText {
            nothing: "This cluster serves no mirrord resource types. Is the operator installed?",
            no_match: "No resource types match the search",
        };
        if state_view::draw(
            frame,
            inner,
            &kinds,
            matching.len(),
            !self.search.phrase().is_empty(),
            &empty,
        ) {
            return;
        }
        let header = ["type", "kind", "group", "version", "scope"].map(str::to_owned);
        let rows: Vec<Vec<String>> = matching
            .iter()
            .map(|kind| {
                vec![
                    kind.plural().to_owned(),
                    kind.resource.kind.clone(),
                    kind.resource.group.clone(),
                    kind.resource.version.clone(),
                    if kind.namespaced {
                        "namespaced"
                    } else {
                        "cluster"
                    }
                    .to_owned(),
                ]
            })
            .collect();
        self.table.draw_dynamic(frame, inner, &header, &rows);
    }

    fn draw_objects(&mut self, frame: &mut Frame, area: Rect, kind: &Kind) {
        let objects = self.objects.clone();
        let objects = objects.read().unwrap();
        let matching = match &objects.listing.state {
            Fetch::Ready(items) => self.matching_items(items),
            _ => Vec::new(),
        };
        let count = objects.listing.count(matching.len());
        let flash = self.current_flash();
        let filter = self.scope.current();
        let block = panel::block(
            kind.plural(),
            count.as_deref(),
            Some(&session_scope::note(&filter, &objects.listing.updated())),
            flash
                .as_ref()
                .map(|(message, style)| (message.as_str(), *style)),
        );
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let searching = !self.search.phrase().is_empty();
        let empty = session_scope::empty_text(
            &filter,
            searching,
            EmptyText {
                nothing: "None in this namespace",
                no_match: "None match the search",
            },
        );
        if state_view::draw(
            frame,
            inner,
            &objects.listing.state,
            matching.len(),
            session_scope::narrowed(&filter, searching),
            &empty,
        ) {
            return;
        }
        let rows: Vec<Vec<String>> = matching.iter().map(|item| item.cells.clone()).collect();
        self.table
            .draw_dynamic(frame, inner, &objects.columns, &rows);
    }
}

fn set_flash(flash: &RwLock<Option<(String, Style, Instant)>>, message: String, style: Style) {
    if let Ok(mut slot) = flash.write() {
        *slot = Some((message, style, Instant::now() + FLASH_FOR));
    }
}

fn connected(client: &Option<anyhow::Result<Client>>) -> Option<Client> {
    client
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .cloned()
}

/// Every listable type in the API groups under [`GROUP_SUFFIX`], at each group's preferred
/// version, sorted by plural, and the groups that did not answer.
///
/// Each group is looked up on its own, so one that is down (the operator's own aggregated API
/// while the operator restarts) does not hide the types of the others.
async fn discover_kinds(client: &Client) -> Result<(Vec<Kind>, Vec<String>), kube::Error> {
    let groups: Vec<String> = client
        .list_api_groups()
        .await?
        .groups
        .into_iter()
        .map(|group| group.name)
        .filter(|name| name.ends_with(GROUP_SUFFIX))
        .collect();

    let answers = join_all(groups.iter().map(|name| discovery::group(client, name))).await;
    let mut kinds = Vec::new();
    let mut unavailable = Vec::new();
    for (name, answer) in groups.iter().zip(answers) {
        match answer {
            Ok(group) => kinds.extend(
                group
                    .recommended_resources()
                    .into_iter()
                    .filter(|(_, capabilities)| capabilities.supports_operation(verbs::LIST))
                    .map(|(resource, capabilities)| Kind {
                        namespaced: capabilities.scope == DiscoveryScope::Namespaced,
                        resource,
                    }),
            ),
            Err(_) => unavailable.push(name.clone()),
        }
    }
    kinds.sort_by(|left, right| left.resource.plural.cmp(&right.resource.plural));
    Ok((kinds, unavailable))
}

/// Lists `kind` in the table format, with a `NAMESPACE` column first when listing every
/// namespace. A server that answers with a plain list instead gets name, namespace and age.
async fn fetch_table(
    client: &Client,
    kind: &Kind,
    namespace: Option<&str>,
) -> Result<(Vec<String>, Vec<Item>), kube::Error> {
    let namespace = namespace.filter(|_| kind.namespaced);
    let url = DynamicObject::url_path(&kind.resource, namespace);
    let mut request = kube::core::Request::new(url)
        .list(&ListParams::default())
        .map_err(kube::Error::BuildRequest)?;
    if let Ok(accept) = TABLE_ACCEPT.parse() {
        request.headers_mut().insert("accept", accept);
    }
    // Each row brings its whole object, not only its name, so "only my sessions" can look for a
    // session's id or key in its spec and status.
    let with_objects = match request.uri().query() {
        Some(_) => format!("{}&includeObject=Object", request.uri()),
        None => format!("{}?includeObject=Object", request.uri()),
    };
    if let Ok(uri) = with_objects.parse() {
        *request.uri_mut() = uri;
    }
    let response: Value = client.request(request).await?;
    let with_namespace = kind.namespaced && namespace.is_none();

    if response.get("kind").and_then(Value::as_str) == Some("Table") {
        return Ok(from_table(&response, with_namespace));
    }
    Ok(from_list(&response, with_namespace))
}

/// Reads a `meta.k8s.io/v1` Table: the columns the type always shows (priority 0), with dates
/// turned into ages the way `kubectl` prints them.
fn from_table(table: &Value, with_namespace: bool) -> (Vec<String>, Vec<Item>) {
    let definitions: Vec<&Value> = table
        .get("columnDefinitions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let shown: Vec<(usize, bool)> = definitions
        .iter()
        .enumerate()
        .filter(|(_, column)| column.get("priority").and_then(Value::as_i64).unwrap_or(0) == 0)
        .map(|(index, column)| {
            let is_date = column.get("type").and_then(Value::as_str) == Some("date")
                || column.get("format").and_then(Value::as_str) == Some("date-time");
            (index, is_date)
        })
        .collect();

    let mut columns: Vec<String> = Vec::new();
    if with_namespace {
        columns.push("namespace".to_owned());
    }
    columns.extend(shown.iter().map(|(index, _)| {
        definitions
            .get(*index)
            .and_then(|column| column.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }));

    let items = table
        .get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|row| {
            let metadata = row.pointer("/object/metadata");
            let name = metadata
                .and_then(|metadata| metadata.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let namespace = metadata
                .and_then(|metadata| metadata.get("namespace"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let raw: Vec<&Value> = row
                .get("cells")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .collect();

            let mut cells = Vec::new();
            if with_namespace {
                cells.push(namespace.clone().unwrap_or_default());
            }
            cells.extend(shown.iter().map(|(index, is_date)| {
                let value = raw.get(*index).copied().unwrap_or(&Value::Null);
                if *is_date {
                    value.as_str().map(age).unwrap_or_default()
                } else {
                    cell(value)
                }
            }));
            Item {
                name,
                namespace,
                cells,
                object: row.get("object").cloned().unwrap_or_default(),
            }
        })
        .collect();
    (columns, items)
}

/// Reads a plain list, for servers without the table format.
fn from_list(list: &Value, with_namespace: bool) -> (Vec<String>, Vec<Item>) {
    let mut columns = Vec::new();
    if with_namespace {
        columns.push("namespace".to_owned());
    }
    columns.extend(["name", "age"].map(str::to_owned));

    let items = list
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|object| {
            let metadata = object.get("metadata");
            let field = |key: &str| {
                metadata
                    .and_then(|metadata| metadata.get(key))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            let name = field("name").unwrap_or_default();
            let namespace = field("namespace");
            let mut cells = Vec::new();
            if with_namespace {
                cells.push(namespace.clone().unwrap_or_default());
            }
            cells.push(name.clone());
            cells.push(
                field("creationTimestamp")
                    .as_deref()
                    .map(age)
                    .unwrap_or_default(),
            );
            Item {
                name,
                namespace,
                cells,
                object: object.clone(),
            }
        })
        .collect();
    (columns, items)
}

fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        value => value.to_string(),
    }
}

/// How long ago `timestamp` was, as `kubectl` prints ages: `45s`, `4m`, `3h25m`, `2d2h`.
fn age(timestamp: &str) -> String {
    let Ok(then) = timestamp.parse::<Timestamp>() else {
        return timestamp.to_owned();
    };
    let seconds = (Timestamp::now().as_second() - then.as_second()).max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h{}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{}d{}h", seconds / 86400, seconds % 86400 / 3600),
    }
}

impl Screen for ResourcesScreen {
    fn new(context: Context) -> Self {
        let kinds = Arc::new(RwLock::new(Fetch::Loading));
        let objects = Arc::new(RwLock::new(Objects::default()));
        let refresh = Arc::new(Notify::new());
        let (open, open_rx) = watch::channel(None);

        let unavailable = Arc::new(RwLock::new(Vec::new()));
        tokio::spawn(Self::discover(
            context.clone(),
            kinds.clone(),
            unavailable.clone(),
        ));
        tokio::spawn(Self::list(
            context.clone(),
            open_rx,
            objects.clone(),
            refresh.clone(),
        ));

        Self {
            scope: SessionScope::new(&context),
            context,
            kinds,
            unavailable,
            open,
            objects,
            table: DataTable::default(),
            search: SearchBox::default(),
            describe: None,
            describing: Arc::default(),
            confirm: None,
            flash: Arc::default(),
            refresh,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(arrived) = self
            .describing
            .write()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            self.describe = Some(arrived);
        }
        if let Some(describe) = &mut self.describe {
            describe.draw(frame, area);
            return;
        }

        let search_height = u16::from(self.search.visible());
        let [body, search] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(search_height)]).areas(area);
        let open = self.open.borrow().clone();
        match open {
            Some(kind) => self.draw_objects(frame, body, &kind),
            None => self.draw_kinds(frame, body),
        }
        if search_height > 0 {
            self.search.draw(frame, search);
        }
        if let Some((_, confirm)) = &self.confirm {
            confirm.draw(frame, area);
        }
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        if let Some(describe) = &mut self.describe {
            if let DescribeOutcome::Closed = describe.handle_event(&event) {
                self.describe = None;
            }
            return ControlFlow::Break(());
        }
        if let Some(((name, namespace), confirm)) = &self.confirm {
            if let Event::Key(key) = event {
                match confirm.handle_key(key) {
                    Answer::Pending => {}
                    Answer::Yes => {
                        let (name, namespace) = (name.clone(), namespace.clone());
                        self.confirm = None;
                        self.delete(name, namespace);
                    }
                    Answer::No => self.confirm = None,
                }
            }
            return ControlFlow::Break(());
        }

        match self.search.handle_event(&event) {
            SearchOutcome::Ignored => {}
            SearchOutcome::Consumed => return ControlFlow::Break(()),
            SearchOutcome::Changed => {
                self.table.select_first();
                return ControlFlow::Break(());
            }
        }

        let Event::Key(key) = &event else {
            return ControlFlow::Continue(event);
        };
        if self.table.handle_key(key) {
            return ControlFlow::Break(());
        }

        let open = self.open.borrow().clone();
        match (key.code, &open) {
            (KeyCode::Enter, None) => {
                let kinds = self.kinds();
                let chosen = self.table.selected().and_then(|index| {
                    self.matching_kinds(&kinds)
                        .get(index)
                        .map(|kind| (*kind).clone())
                });
                if chosen.is_some() {
                    self.show(chosen);
                }
            }
            (KeyCode::Enter, Some(_)) => self.describe_selected(),
            (KeyCode::Esc, Some(_)) => self.show(None),
            (KeyCode::Char('d'), Some(kind)) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some((name, namespace)) = self.selected_item() {
                    let place = namespace
                        .as_deref()
                        .map(|namespace| format!(" in {namespace}"))
                        .unwrap_or_default();
                    self.confirm = Some((
                        (name.clone(), namespace),
                        Confirm::new(
                            format!("Delete {} {name}{place}?", kind.resource.kind),
                            vec![
                                "The operator may act on it being gone, such as ending a session."
                                    .to_owned(),
                            ],
                        ),
                    ));
                }
            }
            (KeyCode::Char('r'), _) => self.refresh.notify_one(),
            _ => return ControlFlow::Continue(event),
        }
        ControlFlow::Break(())
    }

    fn hints(&self) -> Vec<Hint> {
        if let Some(describe) = &self.describe {
            return describe.hints();
        }
        if self.confirm.is_some() {
            return vec![Hint::new("enter", "delete"), Hint::new("esc", "cancel")];
        }
        if self.search.is_editing() {
            return self.search.hints();
        }
        let mut hints = DataTable::hints();
        if self.open.borrow().is_some() {
            hints.push(Hint::new("enter", "describe"));
            hints.push(Hint::new("ctrl-d", "delete"));
            hints.push(Hint::new("esc", "all types"));
        } else {
            hints.push(Hint::new("enter", "open"));
        }
        hints.extend(self.search.hints());
        hints.push(Hint::new("r", "refresh"));
        hints
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{from_list, from_table};

    #[test]
    fn table_columns_come_from_the_server_and_dates_become_ages() {
        let table = json!({
            "kind": "Table",
            "columnDefinitions": [
                { "name": "Name", "type": "string", "priority": 0 },
                { "name": "Target Name", "type": "string", "priority": 0 },
                { "name": "Detail", "type": "string", "priority": 1 },
                { "name": "Age", "type": "date", "priority": 0 }
            ],
            "rows": [{
                "cells": ["kafkajs-consumer", "kafkajs-consumer", "hidden", "2000-01-01T00:00:00Z"],
                "object": { "metadata": { "name": "kafkajs-consumer", "namespace": "test-mirrord" } }
            }]
        });

        let (columns, items) = from_table(&table, true);
        assert_eq!(columns, ["namespace", "Name", "Target Name", "Age"]);
        assert_eq!(items[0].namespace.as_deref(), Some("test-mirrord"));
        assert_eq!(
            &items[0].cells[..3],
            ["test-mirrord", "kafkajs-consumer", "kafkajs-consumer"]
        );
        assert!(items[0].cells[3].ends_with('h'), "{:?}", items[0].cells[3]);
    }

    #[test]
    fn a_plain_list_still_shows_name_and_age() {
        let list =
            json!({ "items": [{ "metadata": { "name": "a", "creationTimestamp": "nonsense" } }] });
        let (columns, items) = from_list(&list, false);
        assert_eq!(columns, ["name", "age"]);
        assert_eq!(items[0].cells, ["a", "nonsense"]);
    }
}
