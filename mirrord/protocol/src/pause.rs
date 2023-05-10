use bincode::{Decode, Encode};

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum PauseTargetResponse {
    Paused,
    AlreadyPaused,
    NoTarget,
}
