use std::{
    fmt,
    ops::{Deref, DerefMut},
};

use bincode::{
    BorrowDecode, Decode, Encode,
    de::{BorrowDecoder, Decoder},
    enc::Encoder,
    error::{DecodeError, EncodeError},
};
use bytes::Bytes;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, SeqAccess, Visitor},
};

/// Custom [`BorrowDecoder`] context for decoding [`Payload`] and types containing [`Payload`].
///
/// Allows for decoding the payload without actually moving the bytes in memory,
/// and still keeping the payload static.
///
/// # How it works
///
/// This context is meant to be used with [`bincode::borrow_decode_from_slice_with_context`].
/// The [`FullData::0`] [`Bytes`] instance **must** contain the slice passed as the first argument.
/// [`BorrowDecode`] implementation of [`Payload`] will first attempt to decode a borrowed byte
/// slice. Then, it will call [`Bytes::slice_ref`] on [`FullData::0`]. This method is cheaped,
/// as it only bumps some refcounts in the [`Bytes`] instance.
///
/// # Panic
///
/// [`bincode::borrow_decode_from_slice_with_context`] will panic if [`FullData::0`] does not
/// contain the slice passed as the first argument.
pub struct FullData(pub Bytes);

/// Wrapper type for [`Bytes`].
///
/// Provides [`Encode`]/[`Decode`]/[`BorrowDecode`]/[`Serialize`]/[`Deserialize`] implementations.
#[derive(PartialEq, Eq, Hash, PartialOrd, Ord, Clone, Default)]
pub struct Payload(pub Bytes);

impl Payload {
    pub fn copy_from<T: AsRef<[u8]> + ?Sized>(data: &T) -> Self {
        Self(Bytes::copy_from_slice(data.as_ref()))
    }
}

impl Encode for Payload {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        self.0.as_ref().encode(encoder)
    }
}

impl<C> Decode<C> for Payload {
    fn decode<D: Decoder<Context = C>>(decoder: &mut D) -> Result<Self, DecodeError> {
        <Vec<u8>>::decode(decoder).map(Bytes::from).map(Self)
    }
}

impl<'de> BorrowDecode<'de, FullData> for Payload {
    fn borrow_decode<D: BorrowDecoder<'de, Context = FullData>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let slice = <&'de [u8]>::borrow_decode(decoder)?;
        let owned = decoder.context().0.slice_ref(slice);
        Ok(Self(owned))
    }
}

impl Serialize for Payload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(self.0.as_ref())
    }
}

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer
            .deserialize_byte_buf(SmartBytesVisitor)
            .map(Self)
    }
}

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} byte(s)", self.0.len())
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

impl<T> From<T> for Payload
where
    Bytes: From<T>,
{
    fn from(value: T) -> Self {
        Self(value.into())
    }
}

impl From<Payload> for Vec<u8> {
    fn from(value: Payload) -> Self {
        value.0.into()
    }
}

/// Used in [`Deserialize`] implementation of [`Payload`].
///
/// [`Vec<u8>`] deserialize implementation extracts it from the [`Deserializer`] byte by byte,
/// using the same implementation as all other [`Vec`]s. This is extremely inefficient.
/// This implementation attempts to pull all bytes at once, if possible.
struct SmartBytesVisitor;

impl<'de> Visitor<'de> for SmartBytesVisitor {
    type Value = Bytes;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("byte array")
    }

    fn visit_seq<V>(self, mut visitor: V) -> Result<Self::Value, V::Error>
    where
        V: SeqAccess<'de>,
    {
        let mut bytes = Vec::with_capacity(visitor.size_hint().unwrap_or(0));
        while let Some(b) = visitor.next_element()? {
            bytes.push(b);
        }
        Ok(Bytes::from(bytes))
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Bytes::copy_from_slice(v))
    }

    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Bytes::from(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Bytes::copy_from_slice(v.as_bytes()))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Bytes::from(v))
    }
}
