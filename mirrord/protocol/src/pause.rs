use bincode::{Decode, Encode};

/// Agent responses for target container pause request.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum PauseTargetResponse {
    /// The target container was paused according to the request.
    Paused,
    /// The target container was already paused when the agent received the request.
    AlreadyPaused,
}
