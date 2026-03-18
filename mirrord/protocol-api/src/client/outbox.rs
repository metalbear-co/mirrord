use std::ops::{Deref, DerefMut};

use mirrord_protocol::ClientMessage;
use smallvec::{SmallVec, smallvec};

/// Container for a batch of outbound [`ClientMessage`]s.
///
/// Produced by the various components of the [`ClientTask`](crate::client::task::ClientTask).
#[must_use = "produced messages should be sent to the server"]
#[derive(Default, Debug, PartialEq, Eq)]
pub struct OutBox(SmallVec<[ClientMessage; 3]>);

impl Deref for OutBox {
    type Target = SmallVec<[ClientMessage; 3]>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for OutBox {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<ClientMessage> for OutBox {
    fn from(value: ClientMessage) -> Self {
        Self(smallvec![value])
    }
}

impl IntoIterator for OutBox {
    type IntoIter = <SmallVec<[ClientMessage; 3]> as IntoIterator>::IntoIter;
    type Item = ClientMessage;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl PartialEq<ClientMessage> for OutBox {
    fn eq(&self, other: &ClientMessage) -> bool {
        match self.as_slice() {
            [msg] => msg == other,
            _ => false,
        }
    }
}

impl PartialEq<&[ClientMessage]> for OutBox {
    fn eq(&self, other: &&[ClientMessage]) -> bool {
        self.as_slice() == *other
    }
}

impl<const N: usize> PartialEq<[ClientMessage; N]> for OutBox {
    fn eq(&self, other: &[ClientMessage; N]) -> bool {
        self.as_slice() == other
    }
}
