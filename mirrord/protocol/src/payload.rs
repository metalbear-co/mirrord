use std::ops::{Deref, DerefMut};

use bincode::{Decode, Encode};
use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Payload is a wrapper for bytes, is used as message body in the protocol and by agent and
/// intproxy as zero copy message payload
#[derive(Eq, PartialEq, Hash, Clone, Default)]
pub struct Payload(pub Bytes);

impl Payload {
    pub fn into_vec(self) -> Vec<u8> {
        self.0.into()
    }
}

impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Payload")
            .field(&format_args!("{} bytes", self.0.len()))
            .finish()
    }
}

/// Convert a type ref to a payload probably cloning the data if self is not copyable
pub trait ToPayload {
    fn to_payload(&self) -> Payload;
}

impl ToPayload for &[u8] {
    fn to_payload(&self) -> Payload {
        Payload(Bytes::from(self.to_vec()))
    }
}

impl<const N: usize> ToPayload for [u8; N] {
    fn to_payload(&self) -> Payload {
        Payload(Bytes::from(self.to_vec()))
    }
}

impl ToPayload for str {
    fn to_payload(&self) -> Payload {
        Payload(Bytes::from(self.to_string().into_bytes()))
    }
}

impl Deref for Payload {
    type Target = Bytes;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Payload {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<B: Into<Bytes>> From<B> for Payload {
    fn from(bytes: B) -> Self {
        Payload(bytes.into())
    }
}

impl Encode for Payload {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), bincode::error::EncodeError> {
        self.0.as_ref().encode(encoder)
    }
}

impl<Context> bincode::Decode<Context> for Payload {
    fn decode<D: bincode::de::Decoder<Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        let bytes: Vec<u8> = Decode::decode(decoder)?;
        Ok(Payload(Bytes::from(bytes)))
    }
}
impl<'de, Context> bincode::BorrowDecode<'de, Context> for Payload {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de, Context = Context>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        let bytes: Vec<u8> = bincode::BorrowDecode::borrow_decode(decoder)?;
        Ok(Payload(Bytes::from(bytes)))
    }
}

impl Serialize for Payload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer).map(Bytes::from)?;
        Ok(Payload(bytes))
    }
}
