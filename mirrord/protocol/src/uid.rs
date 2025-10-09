use std::fmt;

use bincode::{
    BorrowDecode, Decode, Encode,
    de::{BorrowDecoder, Decoder},
    enc::Encoder,
    error::{DecodeError, EncodeError},
};
use uuid::Uuid;

/// [`Uuid`]-based identifier.
#[derive(PartialEq, Eq, Clone, Copy, Hash)]
pub struct Uid(pub Uuid);

impl Uid {
    /// Generates a new identifier with a random v4 [`Uuid`].
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Encode for Uid {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        self.0.as_u128().encode(encoder)
    }
}

impl<C> Decode<C> for Uid {
    fn decode<D: Decoder<Context = C>>(decoder: &mut D) -> Result<Self, DecodeError> {
        u128::decode(decoder).map(Uuid::from_u128).map(Self)
    }
}

impl<'de, C> BorrowDecode<'de, C> for Uid {
    fn borrow_decode<D: BorrowDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        u128::decode(decoder).map(Uuid::from_u128).map(Self)
    }
}

impl fmt::Debug for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for Uid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
