use std::io;

use actix_codec::{Decoder, Encoder};
use bincode::{
    de::BorrowDecode,
    error::{DecodeError, EncodeError},
    Decode, Encode,
};
use bytes::{Buf, BufMut, BytesMut};
use mirrord_config::{agent::AgentConfig, target::TargetConfig};
use mirrord_protocol::{ClientMessage, DaemonMessage};

#[derive(Debug, Clone)]
pub struct AgentInitialize {
    pub agent: AgentConfig,
    pub target: TargetConfig,
}

impl Encode for AgentInitialize {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError> {
        let agent = serde_json::to_vec(&self.agent).expect("agent not json serializable");
        bincode::Encode::encode(&agent, encoder)?;

        let target = serde_json::to_vec(&self.target).expect("target not json serializable");
        bincode::Encode::encode(&target, encoder)?;
        Ok(())
    }
}

impl Decode for AgentInitialize {
    fn decode<D: bincode::de::Decoder>(decoder: &mut D) -> Result<Self, DecodeError> {
        let agent_buffer: Vec<u8> = bincode::Decode::decode(decoder)?;
        let agent = serde_json::from_slice(&agent_buffer).expect("agent not json deserializable");

        let target_buffer: Vec<u8> = bincode::Decode::decode(decoder)?;
        let target =
            serde_json::from_slice(&target_buffer).expect("target not json deserializable");

        Ok(AgentInitialize { agent, target })
    }
}

impl<'de> BorrowDecode<'de> for AgentInitialize {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de>>(
        decoder: &mut D,
    ) -> Result<Self, DecodeError> {
        let agent_buffer: Vec<u8> = bincode::Decode::decode(decoder)?;
        let agent = serde_json::from_slice(&agent_buffer).expect("agent not json deserializable");

        let target_buffer: Vec<u8> = bincode::Decode::decode(decoder)?;
        let target =
            serde_json::from_slice(&target_buffer).expect("target not json deserializable");

        Ok(AgentInitialize { agent, target })
    }
}

#[derive(Encode, Decode, Debug, Clone)]
pub enum OperatorMessage {
    Initialize(AgentInitialize),
    Client(ClientMessage),
    Daemon(DaemonMessage),
}

pub struct OperatorCodec {
    config: bincode::config::Configuration,
}

impl OperatorCodec {
    pub fn new() -> Self {
        OperatorCodec {
            config: bincode::config::standard(),
        }
    }
}

impl Default for OperatorCodec {
    fn default() -> Self {
        OperatorCodec::new()
    }
}

impl Decoder for OperatorCodec {
    type Item = OperatorMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        match bincode::decode_from_slice(&src[..], self.config) {
            Ok((message, read)) => {
                src.advance(read);
                Ok(Some(message))
            }
            Err(DecodeError::UnexpectedEnd { .. }) => Ok(None),
            Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.to_string())),
        }
    }
}

impl Encoder<OperatorMessage> for OperatorCodec {
    type Error = io::Error;

    fn encode(&mut self, msg: OperatorMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded = bincode::encode_to_vec(msg, self.config)
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
        dst.reserve(encoded.len());
        dst.put(&encoded[..]);

        Ok(())
    }
}
