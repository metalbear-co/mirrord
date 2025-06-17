use bincode::{Decode, Encode};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(transparent)]
pub struct Payload(
    #[serde(
        serialize_with = "serialize_payload_inner",
        deserialize_with = "deserialize_payload_inner"
    )]
    Bytes,
);

impl Payload {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    
    pub fn into_vec(self) -> Vec<u8> {
        self.0.into_vec()
    }
}

impl From<Bytes> for Payload {
    fn from(bytes: Bytes) -> Self {
        Payload(bytes)
    }
}

impl AsRef<[u8]> for Payload {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
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

fn serialize_payload_inner<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_bytes(bytes)
}

fn deserialize_payload_inner<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<u8>::deserialize(deserializer).map(Bytes::from)
}
