use mirrord_sessions_manager_protocol::ConnectionAssignment;

#[derive(Debug)]
pub(crate) enum ControlPlaneEvent {
    Assignment(ConnectionAssignment),
}
