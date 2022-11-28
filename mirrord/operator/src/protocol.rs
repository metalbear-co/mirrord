use std::{io, marker::PhantomData};

use actix_codec::{Decoder, Encoder};
use bincode::{
    de::BorrowDecode,
    error::{DecodeError, EncodeError},
    Decode, Encode,
};
use bytes::{Buf, BufMut, BytesMut};
use mirrord_config::target::TargetConfig;
use mirrord_protocol::{ClientMessage, DaemonMessage};

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Handshake {
    target: TargetConfig,
}

impl Handshake {
    pub fn new(target: TargetConfig) -> Self {
        Handshake { target }
    }

    pub fn target(&self) -> &TargetConfig {
        &self.target
    }
}

impl Encode for Handshake {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        let target = serde_json::to_vec(&self.target).expect("target not json serializable");
        bincode::Encode::encode(&target, encoder)?;
        Ok(())
    }
}

impl Decode for Handshake {
    fn decode<D: bincode::de::Decoder>(decoder: &mut D) -> Result<Self, DecodeError> {
        let target_buffer: Vec<u8> = bincode::Decode::decode(decoder)?;
        let target =
            serde_json::from_slice(&target_buffer).expect("target not json deserializable");

        Ok(Handshake { target })
    }
}

impl<'de> BorrowDecode<'de> for Handshake {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let target_buffer: Vec<u8> = bincode::Decode::decode(decoder)?;
        let target =
            serde_json::from_slice(&target_buffer).expect("target not json deserializable");

        Ok(Handshake { target })
    }
}

#[derive(Encode, Decode, Debug, Clone)]
pub enum OperatorRequest {
    Handshake(Handshake),
    Client(ClientMessage),
}

#[derive(Encode, Decode, Debug, Clone)]
pub enum OperatorResponse {
    Daemon(DaemonMessage),
}

pub struct Client;
#[cfg(feature = "server")]
pub struct Server;

pub struct OperatorCodec<T = Client> {
    config: bincode::config::Configuration,
    _type: PhantomData<T>,
}

impl OperatorCodec {
    pub fn client() -> OperatorCodec<Client> {
        OperatorCodec {
            config: bincode::config::standard(),
            _type: PhantomData::<Client>,
        }
    }
}

#[cfg(feature = "server")]
impl OperatorCodec {
    pub fn server() -> OperatorCodec<Server> {
        OperatorCodec {
            config: bincode::config::standard(),
            _type: PhantomData::<Server>,
        }
    }
}

fn bincode_decode<T: Decode>(
    src: &mut BytesMut,
    config: bincode::config::Configuration,
) -> io::Result<Option<T>> {
    match bincode::decode_from_slice(&src[..], config) {
        Ok((message, read)) => {
            src.advance(read);
            Ok(Some(message))
        }
        Err(DecodeError::UnexpectedEnd { .. }) => Ok(None),
        Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.to_string())),
    }
}

fn bincode_encode<T: Encode>(
    msg: T,
    dst: &mut BytesMut,
    config: bincode::config::Configuration,
) -> io::Result<()> {
    let encoded = bincode::encode_to_vec(msg, config)
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
    dst.reserve(encoded.len());
    dst.put(&encoded[..]);

    Ok(())
}

impl Decoder for OperatorCodec<Client> {
    type Item = OperatorResponse;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        bincode_decode::<Self::Item>(src, self.config)
    }
}

impl Encoder<OperatorRequest> for OperatorCodec<Client> {
    type Error = io::Error;

    fn encode(&mut self, msg: OperatorRequest, dst: &mut BytesMut) -> Result<(), Self::Error> {
        bincode_encode(msg, dst, self.config)
    }
}

#[cfg(feature = "server")]
impl Decoder for OperatorCodec<Server> {
    type Item = OperatorRequest;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        bincode_decode::<Self::Item>(src, self.config)
    }
}

#[cfg(feature = "server")]
impl Encoder<OperatorResponse> for OperatorCodec<Server> {
    type Error = io::Error;

    fn encode(&mut self, msg: OperatorResponse, dst: &mut BytesMut) -> Result<(), Self::Error> {
        bincode_encode(msg, dst, self.config)
    }
}
