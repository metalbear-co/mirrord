#![cfg(test)]

use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcp, LayerTcpSteal, StealType},
    DaemonMessage, Port,
};
use tokio::sync::mpsc;

use crate::{
    incoming::MirrorHandle,
    mirror::{MirrorHandleWrapper, TcpMirrorApi},
    steal::{StealerCommand, TcpStealApi},
    util::{protocol_version::SharedProtocolVersion, remote_runtime::BgTaskStatus, ClientId},
};

mod steal_and_mirror;
mod test_service;

async fn get_steal_api_with_subscription(
    client_id: ClientId,
    stealer_tx: mpsc::Sender<StealerCommand>,
    stealer_status: BgTaskStatus,
    protocol_version: Option<&str>,
    steal_type: StealType,
) -> (TcpStealApi, SharedProtocolVersion) {
    let version = SharedProtocolVersion::default();
    if let Some(protocol_version) = protocol_version {
        version.replace(protocol_version.parse().unwrap());
    }

    let mut api = TcpStealApi::new(
        client_id,
        stealer_tx.clone(),
        version.clone(),
        stealer_status,
        8,
    )
    .await
    .unwrap();

    let port = steal_type.get_port();
    api.handle_client_message(LayerTcpSteal::PortSubscribe(steal_type))
        .await
        .unwrap();
    let message = api.recv().await.unwrap();
    assert_eq!(
        message,
        DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(port)))
    );

    (api, version)
}

async fn get_mirror_api_with_subscription(
    mirror_handle: MirrorHandle,
    protocol_version: Option<&str>,
    port: Port,
) -> (MirrorHandleWrapper, SharedProtocolVersion) {
    let version = SharedProtocolVersion::default();
    if let Some(protocol_version) = protocol_version {
        version.replace(protocol_version.parse().unwrap());
    }

    let mut mirror_api = MirrorHandleWrapper::new(mirror_handle, version.clone());

    mirror_api
        .handle_client_message(LayerTcp::PortSubscribe(port))
        .await
        .unwrap();
    let message = mirror_api.recv().await.unwrap().unwrap();
    assert_eq!(
        message,
        DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(port)))
    );

    (mirror_api, version)
}
