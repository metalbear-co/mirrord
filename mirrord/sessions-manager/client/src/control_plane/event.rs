use mirrord_sessions_manager_protocol::{Admission, ConnectionAssignment, TaskEnded, TaskMatch};

/// An event received from the sessions-manager assignment SSE stream.
///
/// The stream attaches to a durable logical registration identified by an agent instance ID or
/// intproxy session ID. It can therefore reconnect and receive still-unclaimed assignments, but
/// it must stop when the server has replaced this attachment with a newer one for the same
/// identity.
#[derive(Debug)]
pub(crate) enum ControlPlaneEvent {
    /// Offers one side of an allocated data-plane connection.
    ///
    /// The assignment contains the role-bound WebSocket endpoint and one-use authorization needed
    /// to claim that side of the allocation. Receiving this event does not consume it: the server
    /// retains it until the client successfully claims the data plane, so it can be replayed after
    /// an interrupted SSE stream or failed connection attempt.
    Assignment(ConnectionAssignment),

    /// Ends this SSE attachment because a newer attachment registered with the same stable
    /// identity.
    ///
    /// This differs from a transport interruption. Retrying it would supersede the newer
    /// attachment and can make two clients repeatedly replace each other, so the subscriber must
    /// treat this event as terminal.
    Superseded,

    /// A task matched this watching registration. Only a watch receives it.
    #[expect(
        dead_code,
        reason = "decoded so a subscriber can receive it; no subscriber reads it yet"
    )]
    Matched(TaskMatch),

    /// A task this watching registration matched is gone. Only a watch receives it.
    #[expect(
        dead_code,
        reason = "decoded so a subscriber can receive it; no subscriber reads it yet"
    )]
    Ended(TaskEnded),

    /// The verdict on this gated agent's task. Only an agent registered with a pending gate
    /// receives it.
    #[expect(
        dead_code,
        reason = "decoded so a subscriber can receive it; no subscriber reads it yet"
    )]
    Admission(Admission),
}
