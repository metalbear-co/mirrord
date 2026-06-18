use std::{env, error::Error, net::SocketAddr, time::Duration};

use mirrord_config::{
    LayerFileConfig, MIRRORD_AGENT_SIDECAR_ADDR, MIRRORD_LAYER_INTPROXY_ADDR,
    config::{ConfigContext, MirrordConfig},
    feature::fs::READONLY_FILE_BUFFER_DEFAULT,
};
use mirrord_intproxy::{IntProxy, IntProxyIntervals, session_monitor::MonitorTx};
use tokio::net::TcpListener;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn parse_socket_addr(name: &str) -> Result<SocketAddr, Box<dyn Error>> {
    let value = env::var(name)?;
    Ok(value.parse()?)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_thread_ids(true)
                .with_span_events(fmt::format::FmtSpan::NEW | fmt::format::FmtSpan::CLOSE),
        )
        .with(EnvFilter::from_default_env())
        .init();

    let layer_addr = parse_socket_addr(MIRRORD_LAYER_INTPROXY_ADDR)?;
    let agent_addr = parse_socket_addr(MIRRORD_AGENT_SIDECAR_ADDR)?;

    tracing::info!(%layer_addr, %agent_addr, "Starting remote intproxy");

    let listener = TcpListener::bind(layer_addr).await?;

    let mut config_context = ConfigContext::default();
    let resolved_config = LayerFileConfig::default().generate_config(&mut config_context)?;
    let tls_delivery = resolved_config
        .feature
        .network
        .incoming
        .tls_delivery
        .unwrap_or_default();
    let experimental = resolved_config.experimental;

    let intproxy = IntProxy::new_for_raw_address(
        agent_addr,
        listener,
        READONLY_FILE_BUFFER_DEFAULT,
        tls_delivery,
        IntProxyIntervals {
            ping: Duration::from_secs(30),
            process_logging: Duration::from_secs(60),
        },
        &experimental,
        MonitorTx::disabled(),
    )
    .await?;

    intproxy
        .run(Duration::from_secs(30), Duration::from_secs(5))
        .await?;

    Ok(())
}
