use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use tracing::error;

use super::*;

/// Created once per agent during initialization.
///
/// Runs as a separate thread while the agent lives.
///
/// - (agent -> stealer) communication is handled by [`command_rx`];
/// - (stealer -> agent) communication is handled by [`client_senders`], and the [`Sender`] channels
///   come inside [`StealerCommand`]s through  [`command_rx`];
pub(crate) struct TcpConnectionStealer {
    port_subscriptions: Subscriptions<Port, ClientID>,

    /// Communication between (agent -> stealer) task.
    ///
    /// The agent controls the stealer task through [`TcpStealerAPI::command_tx`].
    command_rx: Receiver<StealerCommand>,

    /// Connected clients (layer instances) and the channels which the stealer task uses to send
    /// back messages (stealer -> agent -> layer).
    clients: HashMap<ClientID, Sender<DaemonTcp>>,
    index_allocator: IndexAllocator<ConnectionId>,

    /// Intercepts the connections, instead of letting them go through their normal pathways, this
    /// is used to steal the traffic.
    // TODO(alex) [mid] 2022-12-02: Is 1 listener enough? Do we need to create 1 per connection?
    // 1 per layer? 1 per port???
    stealer: TcpListener,
    iptables: SafeIpTables<iptables::IPTables>,
}

impl TcpConnectionStealer {
    #[tracing::instrument(level = "debug")]
    pub(crate) async fn new(
        command_rx: Receiver<StealerCommand>,
        pid: Option<u64>,
    ) -> Result<Self, AgentError> {
        if let Some(pid) = pid {
            let namespace = PathBuf::from("/proc")
                .join(PathBuf::from(pid.to_string()))
                .join(PathBuf::from("ns/net"));

            set_namespace(namespace).unwrap();
        }

        Ok(Self {
            port_subscriptions: Subscriptions::new(),
            command_rx,
            clients: HashMap::with_capacity(8),
            index_allocator: IndexAllocator::new(),
            stealer: TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?,
            iptables: SafeIpTables::new(iptables::new(false).unwrap())?,
        })
    }

    // TODO(alex) [low] 2022-12-01: Better docs.
    /// Runs the stealer loop.
    ///
    /// The loop deals with 3 different paths:
    ///
    /// 1. Receiving [`StealerCommand`]s and calling [`TcpConnectionStealer::handle_command`];
    ///
    /// 2. Controlling a listener? Stream? The "socket" that stealer users to take the traffic;
    ///
    /// 3. Handling the cancellation of the whole stealer thread.
    #[tracing::instrument(level = "debug", skip(self))]
    pub(crate) async fn start(
        mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), AgentError> {
        loop {
            select! {
                command = self.command_rx.recv() => {
                    if let Some(command) = command {
                        self.handle_command(command).await?;
                    } else { break; }
                },
                // TODO(alex) [mid] 2022-12-01: This should be global as well?
                //
                // Like steal everything or only steal what the users asked for?
                //
                // If we do this, then we break stuff, as this would mean we take every `TcpStream`.
                //
                // ADD(alex) [mid] 2022-12-02: This will only steal if we have added a
                // redirection rule, so I think it's safe (only starts capturing after someone sends
                // a `Subscribe` message to stealer).
                accept = self.stealer.accept() => {
                    match accept {
                        Ok((stealer_stream, address)) => {
                            // TODO(alex) [high] 2022-12-02: Now we need to implement this, but the
                            // question remains: do we need to keep the `self.stealer`, or do we
                            // want to keep the `stealer_stream`?
                            self.incoming_connection(stealer_stream, address).await?;
                        }
                        Err(fail) => {
                            error!("Something went wrong while accepting a connection {:#?}", fail);
                            break;
                        }
                    }
                }
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }

        Ok(())
    }

    /// Registers a new layer instance that has the `steal` feature enabled.
    #[tracing::instrument(level = "debug", skip(self, sender))]
    fn new_client(&mut self, client_id: ClientID, sender: Sender<DaemonTcp>) {
        self.clients.insert(client_id, sender);
    }

    /// Helper function to handle [`LayerTcpSteal::PortSubscribe`] messages.
    ///
    /// Inserts `port` into [`TcpConnectionStealer::iptables`] rules, and subscribes the layer with
    /// `client_id` to steal traffic from it.
    #[tracing::instrument(level = "debug", skip(self))]
    async fn port_subscribe(&mut self, client_id: ClientID, port: Port) -> Result<(), AgentError> {
        // TODO(alex) [mid] 2022-12-02: We only check if this `client_id` is already subscribed to
        // this port, but other clients might be subscribed to it.
        let ports = self.port_subscriptions.get_client_topics(client_id);

        if ports.contains(&port) {
            warn!("Port {port:?} is already subscribed for client {client_id:?}!");
            Ok(())
        } else {
            self.port_subscriptions.subscribe(client_id, port);
            self.iptables
                .add_redirect(port, self.stealer.local_addr()?.port())?;

            self.send_message_to_single_client(&client_id, DaemonTcp::Subscribed)
                .await
        }
    }

    /// Helper function to handle [`LayerTcpSteal::PortUnsubscribe`] messages.
    ///
    /// Removes `port` from [`TcpConnectionStealer::iptables`] rules, and unsubscribes the layer
    /// with `client_id`.
    #[tracing::instrument(level = "debug", skip(self))]
    fn port_unsubscribe(&mut self, client_id: ClientID, port: Port) -> Result<(), AgentError> {
        self.port_subscriptions
            .get_client_topics(client_id)
            .iter()
            .find_map(|subscribed_port| {
                (*subscribed_port == port).then(|| {
                    self.iptables
                        .remove_redirect(port, self.stealer.local_addr()?.port())
                })
            })
            .transpose()?;

        self.port_subscriptions.unsubscribe(client_id, port);

        Ok(())
    }

    /// Removes the client with `client_id`, and also removes their redirection rules from
    /// [`TcpConnectionStealer::iptables`].
    #[tracing::instrument(level = "debug", skip(self))]
    fn close_client(&mut self, client_id: ClientID) -> Result<(), AgentError> {
        self.clients.remove(&client_id);

        let stealer_port = self.stealer.local_addr()?.port();
        let ports = self.port_subscriptions.get_client_topics(client_id);
        ports
            .into_iter()
            .try_for_each(|port| self.iptables.remove_redirect(port, stealer_port))?;

        self.port_subscriptions.remove_client(client_id);

        Ok(())
    }

    /// Sends a [`DaemonTcp`] message back to the client with `client_id`.
    #[tracing::instrument(level = "debug", skip(self))]
    async fn send_message_to_single_client(
        &mut self,
        client_id: &ClientID,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        if let Some(sender) = self.clients.get(client_id) {
            sender.send(message).await.map_err(|fail| {
                warn!(
                    "Failed to send message to client {} with {:#?}!",
                    client_id, fail
                );
                let _ = self.close_client(*client_id);
                fail
            })?;
        }

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self, clients))]
    async fn send_message_to_subscribed_clients(
        &mut self,
        clients: impl Iterator<Item = &(ClientID, Sender<DaemonTcp>)>,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        for (client_id, sender) in clients {
            sender.send(message.clone()).await.map_err(|fail| {
                warn!(
                    "Failed to send message to client {} with {:#?}!",
                    client_id, fail
                );
                let _ = self.close_client(*client_id);
                fail
            })?;
        }
        Ok(())
    }

    /// Handles [`Command`]s that were received by [`TcpConnectionStealer::command_rx`].
    #[tracing::instrument(level = "debug", skip(self))]
    async fn handle_command(&mut self, command: StealerCommand) -> Result<(), AgentError> {
        let StealerCommand { client_id, command } = command;

        match command {
            Command::NewClient(daemon_tx) => self.new_client(client_id, daemon_tx),
            Command::Subscribe(port) => self.port_subscribe(client_id, port).await?,
            Command::UnsubscribePort(port) => self.port_unsubscribe(client_id, port)?,
            Command::AgentClosed => todo!(),
        }

        Ok(())
    }
}
