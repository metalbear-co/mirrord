use super::*;

/// Created once per agent during initialization.
///
/// Runs as a separate thread while the agent lives.
///
/// - (agent -> stealer) communication is handled by [`command_rx`];
/// - (stealer -> agent) communication is handled by [`client_senders`], and the [`Sender`] channels
///   come inside [`StealerCommand`]s through  [`command_rx`];
#[derive(Debug)]
pub(crate) struct TcpConnectionStealer {
    port_subscriptions: Subscriptions<Port, ClientID>,

    /// Communication between (agent -> stealer) task.
    ///
    /// The agent controls the stealer task through [`TcpStealerAPI::command_tx`].
    command_rx: Receiver<StealerCommand>,

    /// Connected clients (layer instances) and the channels which the stealer task uses to send
    /// back messages (stealer -> agent -> layer).
    client_senders: HashMap<ClientID, Sender<DaemonTcp>>,
    index_allocator: IndexAllocator<ConnectionId>,
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
            client_senders: HashMap::with_capacity(8),
            index_allocator: IndexAllocator::new(),
        })
    }

    // TODO(alex) [low] 2022-12-01: Better docs.
    /// Runs the stealer loop.
    #[tracing::instrument(level = "trace", skip(self))]
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
                // accept = listener.accept() => todo!()
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self, sender))]
    fn new_client(&mut self, client_id: ClientID, sender: Sender<DaemonTcp>) {
        self.client_senders.insert(client_id, sender);
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn subscribe(&mut self, client_id: ClientID, port: Port) -> Result<(), AgentError> {
        self.port_subscriptions.subscribe(client_id, port);
        // self.update_stealer()?;
        self.send_message_to_single_client(&client_id, DaemonTcp::Subscribed)
            .await
    }

    /// Removes the client with `client_id`, and also unsubscribes its port.
    #[tracing::instrument(level = "trace", skip(self))]
    fn close_client(&mut self, client_id: ClientID) -> Result<(), AgentError> {
        self.client_senders.remove(&client_id);
        self.port_subscriptions.remove_client(client_id);
        // self.update_sniffer()
        todo!()
    }

    /// Sends a [`DaemonTcp`] message back to the client with `client_id`.
    // #[tracing::instrument(level = "trace", skip(self))]
    async fn send_message_to_single_client(
        &mut self,
        client_id: &ClientID,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        if let Some(sender) = self.client_senders.get(client_id) {
            sender.send(message).await.map_err(|err| {
                warn!(
                    "Failed to send message to client {} with {:#?}!",
                    client_id, err
                );
                let _ = self.close_client(*client_id);
                err
            })?;
        }

        Ok(())
    }

    // #[tracing::instrument(level = "trace", skip(self, clients))]
    async fn send_message_to_all_clients(
        &mut self,
        clients: impl Iterator<Item = &ClientID>,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        for client_id in clients {
            self.send_message_to_single_client(client_id, message.clone())
                .await?;
        }
        Ok(())
    }

    // #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_command(&mut self, command: StealerCommand) -> Result<(), AgentError> {
        let StealerCommand { client_id, command } = command;

        match command {
            Command::NewAgent(daemon_tx) => todo!(),
            Command::Subscribe(port) => todo!(),
            Command::UnsubscribePort(port) => todo!(),
            Command::AgentClosed => todo!(),
        }

        Ok(())
    }
}
