use futures::{future, Sink, SinkExt, Stream};
use mirrord_protocol::vpn::NetworkConfiguration;
use tokio::io;

use crate::packet::patch_packet_checksum;

pub fn create_vpn_socket(
    network: &NetworkConfiguration,
) -> impl Stream<Item = io::Result<Vec<u8>>> + Sink<Vec<u8>, Error = io::Error> {
    let mut config = tun2::Configuration::default();
    config
        .address(network.ip)
        .netmask(network.net_mask)
        .destination(network.gateway)
        .up();

    #[cfg(target_os = "linux")]
    config.platform_config(|config| {
        config.ensure_root_privileges(true);
    });

    let dev = tun2::create_as_async(&config).unwrap();

    dev.into_framed().with(|mut packet: Vec<u8>| {
        patch_packet_checksum(&mut packet);
        future::ready(Ok::<_, io::Error>(packet))
    })
}
