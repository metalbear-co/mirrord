//! Background data fetch: watches every `PreviewSession` in the cluster into a live
//! [`reflector::Store`], and groups them into a namespace -> target -> sessions tree.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use futures_util::StreamExt;
use kube::{
    Api, ResourceExt,
    runtime::{
        WatchStreamExt, reflector,
        watcher::{self, watcher},
    },
};
use mirrord_operator::crd::preview::PreviewSession;

use super::ui::TargetKey;
use crate::context::Context;

/// Namespace -> target -> preview sessions, alphabetically ordered at every level (via
/// `BTreeMap`).
#[derive(Default)]
pub struct PreviewEnvsTree(pub BTreeMap<String, BTreeMap<TargetKey, Vec<Arc<PreviewSession>>>>);

impl PreviewEnvsTree {
    pub fn build(sessions: Vec<Arc<PreviewSession>>) -> Self {
        let mut namespaces: BTreeMap<String, BTreeMap<TargetKey, Vec<Arc<PreviewSession>>>> =
            BTreeMap::new();

        for session in sessions {
            let namespace = session.namespace().unwrap_or_default();
            // Grouped by (kind, name) — the workload — not by container, which is a
            // card-level detail rather than a distinct group.
            let target = (
                session.spec.target.kind.clone(),
                session.spec.target.name.clone(),
            );
            namespaces
                .entry(namespace)
                .or_default()
                .entry(target)
                .or_default()
                .push(session);
        }

        for targets in namespaces.values_mut() {
            for envs in targets.values_mut() {
                envs.sort_by_key(|session| session.name_any());
            }
        }

        Self(namespaces)
    }
}

/// Watches every `PreviewSession` in the cluster into a live [`reflector::Store`], restarting
/// the watch whenever the connection scope changes (a new client is pushed onto
/// `context.client`).
pub async fn run(
    context: Context,
    data: Arc<RwLock<Option<anyhow::Result<reflector::Store<PreviewSession>>>>>,
) {
    let mut client_rx = context.client.clone();

    loop {
        let client = loop {
            // `anyhow::Error` isn't `Clone`, so extract what's needed (an owned `Client`, or
            // the error's message) before dropping the borrow, rather than cloning the whole
            // `Option<anyhow::Result<Client>>`.
            let outcome = {
                let snapshot = client_rx.borrow_and_update();
                match &*snapshot {
                    Some(Ok(client)) => Some(Ok(client.clone())),
                    Some(Err(error)) => Some(Err(error.to_string())),
                    None => None,
                }
            };

            match outcome {
                Some(Ok(client)) => break client,
                Some(Err(message)) => {
                    *data.write().unwrap() = Some(Err(anyhow::anyhow!(message)));
                    context.redraw.notify_one();
                }
                None => {}
            }

            if client_rx.changed().await.is_err() {
                return;
            }
        };

        let (store, writer) = reflector::store::<PreviewSession>();
        *data.write().unwrap() = Some(Ok(store));
        context.redraw.notify_one();

        let api: Api<PreviewSession> = Api::all(client);
        let stream = watcher(api, watcher::Config::default())
            .default_backoff()
            .reflect(writer);
        tokio::pin!(stream);

        loop {
            tokio::select! {
                event = stream.next() => match event {
                    // Resync starting; nothing changed yet, so no redraw needed.
                    Some(Ok(watcher::Event::Init)) => {}
                    Some(Ok(_)) => context.redraw.notify_one(),
                    Some(Err(error)) => tracing::warn!(
                        error = &error as &dyn std::error::Error,
                        "preview session watch error",
                    ),
                    None => break, // stream ended unexpectedly; rebuild the watcher
                },
                changed = client_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    break; // client changed (e.g. reconnect); rebuild against the new client
                }
            }
        }
    }
}
