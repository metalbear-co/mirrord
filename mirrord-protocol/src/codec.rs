use std::{
    io::{self, SeekFrom},
    net::IpAddr,
    path::PathBuf,
};

use actix_codec::{Decoder, Encoder};
use bincode::{error::DecodeError, Decode, Encode};
use bytes::{Buf, BufMut, BytesMut};

pub type ConnectionID = u16;
pub type Port = u16;

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub struct NewTCPConnection {
    pub connection_id: ConnectionID,
    pub address: IpAddr,
    pub destination_port: Port,
    pub source_port: Port,
}

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub struct TCPData {
    pub connection_id: ConnectionID,
    pub data: Vec<u8>,
}

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub struct TCPClose {
    pub connection_id: ConnectionID,
}

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub struct LogMessage {
    pub message: String,
}

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub struct ReadFileRequest {
    pub fd: i32,
    pub buffer_size: usize,
}

/// Alternative to `std::io::SeekFrom`, used to implement `bincode::Encode` and `bincode::Decode`.
#[derive(Encode, Decode, Debug, PartialEq, Clone, Copy, Eq)]
pub enum SeekFromInternal {
    Start(u64),
    End(i64),
    Current(i64),
}

impl const From<SeekFromInternal> for SeekFrom {
    fn from(seek_from: SeekFromInternal) -> Self {
        match seek_from {
            SeekFromInternal::Start(start) => SeekFrom::Start(start),
            SeekFromInternal::End(end) => SeekFrom::End(end),
            SeekFromInternal::Current(current) => SeekFrom::Current(current),
        }
    }
}

impl const From<SeekFrom> for SeekFromInternal {
    fn from(seek_from: SeekFrom) -> Self {
        match seek_from {
            SeekFrom::Start(start) => SeekFromInternal::Start(start),
            SeekFrom::End(end) => SeekFromInternal::End(end),
            SeekFrom::Current(current) => SeekFromInternal::Current(current),
        }
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub struct SeekFileRequest {
    pub fd: i32,
    pub seek_from: SeekFromInternal,
}

/// `-layer` --> `-agent` messages.
#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub enum ClientMessage {
    PortSubscribe(Vec<u16>),
    Close,
    ConnectionUnsubscribe(ConnectionID),
    OpenFileRequest(PathBuf),
    ReadFileRequest(ReadFileRequest),
    SeekFileRequest(SeekFileRequest),
}

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub struct OpenFileResponse {
    pub fd: i32,
}

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub struct ReadFileResponse {
    pub bytes: Vec<u8>,
    pub read_amount: usize,
}

#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub struct SeekFileResponse {
    pub result_offset: u64,
}

/// `-agent` --> `-layer` messages.
#[derive(Encode, Decode, Debug, PartialEq, Clone)]
pub enum DaemonMessage {
    Close,
    NewTCPConnection(NewTCPConnection),
    TCPData(TCPData),
    TCPClose(TCPClose),
    LogMessage(LogMessage),
    OpenFileResponse(OpenFileResponse),
    ReadFileResponse(ReadFileResponse),
    SeekFileResponse(SeekFileResponse),
}

pub struct ClientCodec {
    config: bincode::config::Configuration,
}

impl ClientCodec {
    pub fn new() -> Self {
        ClientCodec {
            config: bincode::config::standard(),
        }
    }
}

impl Default for ClientCodec {
    fn default() -> Self {
        ClientCodec::new()
    }
}

impl Decoder for ClientCodec {
    type Item = DaemonMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        match bincode::decode_from_slice(&src[..], self.config) {
            Ok((message, read)) => {
                src.advance(read);
                Ok(Some(message))
            }
            Err(DecodeError::UnexpectedEnd) => Ok(None),
            Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.to_string())),
        }
    }
}

impl Encoder<ClientMessage> for ClientCodec {
    type Error = io::Error;

    fn encode(&mut self, msg: ClientMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded = match bincode::encode_to_vec(msg, self.config) {
            Ok(encoded) => encoded,
            Err(err) => {
                return Err(io::Error::new(io::ErrorKind::Other, err.to_string()));
            }
        };
        dst.reserve(encoded.len());
        dst.put(&encoded[..]);

        Ok(())
    }
}

pub struct DaemonCodec {
    config: bincode::config::Configuration,
}

impl DaemonCodec {
    pub fn new() -> Self {
        DaemonCodec {
            config: bincode::config::standard(),
        }
    }
}

impl Default for DaemonCodec {
    fn default() -> Self {
        DaemonCodec::new()
    }
}

impl Decoder for DaemonCodec {
    type Item = ClientMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        match bincode::decode_from_slice(&src[..], self.config) {
            Ok((message, read)) => {
                src.advance(read);
                Ok(Some(message))
            }
            Err(DecodeError::UnexpectedEnd) => Ok(None),
            Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.to_string())),
        }
    }
}

impl Encoder<DaemonMessage> for DaemonCodec {
    type Error = io::Error;

    fn encode(&mut self, msg: DaemonMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded = match bincode::encode_to_vec(msg, self.config) {
            Ok(encoded) => encoded,
            Err(err) => {
                return Err(io::Error::new(io::ErrorKind::Other, err.to_string()));
            }
        };
        dst.reserve(encoded.len());
        dst.put(&encoded[..]);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;

    use super::*;

    #[test]
    fn sanity_client_encode_decode() {
        let mut client_codec = ClientCodec::new();
        let mut daemon_codec = DaemonCodec::new();
        let mut buf = BytesMut::new();

        let msg = ClientMessage::PortSubscribe(vec![1, 2, 3]);

        client_codec.encode(msg.clone(), &mut buf).unwrap();

        let decoded = daemon_codec.decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded, msg);
        assert!(buf.is_empty());
    }

    #[test]
    fn sanity_daemon_encode_decode() {
        let mut client_codec = ClientCodec::new();
        let mut daemon_codec = DaemonCodec::new();
        let mut buf = BytesMut::new();

        let msg = DaemonMessage::TCPData(TCPData {
            connection_id: 1,
            data: vec![1, 2, 3],
        });

        daemon_codec.encode(msg.clone(), &mut buf).unwrap();

        let decoded = client_codec.decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded, msg);
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_client_invalid_data() {
        let mut codec = ClientCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u8(254);

        let res = codec.decode(&mut buf);
        match res {
            Ok(_) => panic!("Should have failed"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::Other),
        }
    }

    #[test]
    fn decode_client_partial_data() {
        let mut codec = ClientCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u8(1);

        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn decode_daemon_invalid_data() {
        let mut codec = DaemonCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u8(254);

        let res = codec.decode(&mut buf);
        match res {
            Ok(_) => panic!("Should have failed"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::Other),
        }
    }

    #[test]
    fn decode_daemon_partial_data() {
        let mut codec = DaemonCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u8(0);

        assert!(codec.decode(&mut buf).unwrap().is_none());
    }
}
