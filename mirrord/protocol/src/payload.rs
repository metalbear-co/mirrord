use std::{borrow::Borrow, slice::SliceIndex};

use bincode::{Decode, Encode};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// Payload is a wrapper for bytes, is used as message body in the protocol and by agent and
/// intproxy as zero copy message payload
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
        (*self.0.as_ref()).into()
    }

    pub fn into_bytes(self) -> Bytes {
        self.0
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr()
    }

    pub fn get<I>(&self, index: I) -> Option<&[u8]>
    where
        I: SliceIndex<[u8], Output = [u8]>,
    {
        self.0.as_ref().get(index)
    }
}

/// Convert a type ref to a payload probably cloning the data if self is not copyable
pub trait ToPayload {
    fn to_payload(&self) -> Payload;
}

/// Convert a type to a payload consuming self
pub trait IntoPayload {
    fn into_payload(self) -> Payload;
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

impl<'a> IntoPayload for Vec<u8> {
    fn into_payload(self) -> Payload {
        Payload(Bytes::from(self))
    }
}

impl<'a> ToPayload for str {
    fn to_payload(&self) -> Payload {
        Payload(Bytes::from(self.to_string().into_bytes()))
    }
}

impl IntoIterator for Payload {
    type Item = u8;
    type IntoIter = bytes::buf::IntoIter<Bytes>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<B: for<'a> Into<Bytes>> From<B> for Payload {
    fn from(bytes: B) -> Self {
        Payload(bytes.into())
    }
}

impl Borrow<[u8]> for Payload {
    fn borrow(&self) -> &[u8] {
        &self.0
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

/// this method is used to serialize the payload inner bytes, used by serde
fn serialize_payload_inner<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_bytes(bytes)
}

/// this method is used to deserialize the payload inner bytes, used by serde
fn deserialize_payload_inner<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<u8>::deserialize(deserializer).map(Bytes::from)
}
