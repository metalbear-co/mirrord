use bincode::{
    BorrowDecode, Decode, Encode,
    de::{BorrowDecoder, Decoder},
    enc::Encoder,
    error::{DecodeError, EncodeError},
};
use derive_more::{Display, From};
use uuid::Uuid;

/// Generic unique ID.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, Display, From)]
pub struct Uid(pub Uuid);

impl Uid {
    /// Generates a new random v4 UID.
    ///
    /// Generally, random v4 UIDs are considered to be "safe",
    /// as collision probability is negligible.
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Encode for Uid {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        // `Uuid::as_u128` takes endianess into consideration.
        self.0.as_u128().encode(encoder)
    }
}

impl<C> Decode<C> for Uid {
    fn decode<D: Decoder<Context = C>>(decoder: &mut D) -> Result<Self, DecodeError> {
        // `Uuid::from_u128` takes endianess into consideration.
        u128::decode(decoder).map(Uuid::from_u128).map(Self)
    }
}

impl<'de, C> BorrowDecode<'de, C> for Uid {
    fn borrow_decode<D: BorrowDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        // `Uuid::from_u128` takes endianess into consideration.
        u128::borrow_decode(decoder).map(Uuid::from_u128).map(Self)
    }
}
