//! Protocol between an internal proxy running in a sidecar container and the external proxy on
//! the container host.
//!
//! The external proxy relays the agent connection of the internal proxy, so most of what the two
//! exchange are `mirrord-protocol` messages. Wrapping them in frames of this protocol lets the
//! two proxies also exchange messages of their own, which the agent never sees.
//!
//! Only the direction from the internal proxy is framed. A stream starts with [`PREAMBLE`] and
//! continues with frames, each a tag byte followed by a payload:
//!
//! | tag             | payload                                    |
//! |-----------------|--------------------------------------------|
//! | [`AGENT_FRAME`] | one encoded [`ClientMessage`] for the agent |
//!
//! The direction from the external proxy carries plain [`DaemonMessage`]s.
//!
//! The image of the sidecar is configurable, so the two proxies can come from different mirrord
//! versions, one of which does not know this protocol:
//! * The internal proxy uses it only when the connect info it was started with says so, which only
//!   a CLI whose external proxy understands it does.
//! * The external proxy recognizes an internal proxy that sends plain [`ClientMessage`]s by the
//!   missing [`PREAMBLE`].

use std::io;

use actix_codec::{Decoder, Encoder, Framed};
use bytes::{Buf, BufMut, BytesMut};
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage, ProtocolCodec};
use mirrord_protocol_io::{AsyncIO, Client, Connection};

/// Opens a stream that uses this protocol.
///
/// A `mirrord-protocol` message starts with the index of its enum variant, which bincode never
/// encodes with a leading `0xFF`, so no stream of plain [`ClientMessage`]s starts like this.
const PREAMBLE: [u8; 8] = *b"\xFFmirrord";

/// Tag of a frame that holds a [`ClientMessage`] to pass on to the agent.
const AGENT_FRAME: u8 = 0;

/// Message of an internal proxy to the external proxy.
#[derive(Debug, PartialEq, Eq)]
pub enum SidecarMessage {
    /// Message to pass on to the agent.
    Agent(ClientMessage),
}

/// Makes the agent connection of an internal proxy over `io`, a stream to the external proxy.
///
/// `sidecar_protocol` tells whether the external proxy expects this protocol or plain
/// `mirrord-protocol`.
pub fn connect<IO>(io: IO, sidecar_protocol: bool) -> Connection<Client>
where
    IO: AsyncIO + Unpin,
{
    if sidecar_protocol {
        Connection::from_channel(Framed::new(io, InternalProxyCodec::default()))
    } else {
        Connection::from_stream(io)
    }
}

/// Codec for the end of the internal proxy.
///
/// Encodes [`ClientMessage`]s that are already encoded for the agent, as that is what
/// [`Connection`] hands to its transport.
#[derive(Default)]
struct InternalProxyCodec {
    preamble_sent: bool,
}

impl Encoder<Vec<u8>> for InternalProxyCodec {
    type Error = io::Error;

    fn encode(&mut self, client_message: Vec<u8>, dst: &mut BytesMut) -> io::Result<()> {
        if !self.preamble_sent {
            dst.put_slice(&PREAMBLE);
            self.preamble_sent = true;
        }

        dst.reserve(client_message.len() + 1);
        dst.put_u8(AGENT_FRAME);
        dst.put_slice(&client_message);

        Ok(())
    }
}

impl Decoder for InternalProxyCodec {
    type Item = DaemonMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<DaemonMessage>> {
        ProtocolCodec::<DaemonMessage, ()>::default().decode(src)
    }
}

/// What the internal proxy on the other end of a stream speaks.
#[derive(Default, Clone, Copy)]
enum Peer {
    /// Not known before the first bytes of the stream arrive.
    #[default]
    Unknown,
    /// Plain [`ClientMessage`]s.
    Plain,
    /// The protocol of this module.
    Sidecar,
}

/// Codec for the end of the external proxy.
#[derive(Default)]
pub struct ExternalProxyCodec {
    peer: Peer,
}

impl Decoder for ExternalProxyCodec {
    type Item = SidecarMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<SidecarMessage>> {
        if matches!(self.peer, Peer::Unknown) {
            if src
                .iter()
                .zip(PREAMBLE)
                .any(|(received, expected)| *received != expected)
            {
                self.peer = Peer::Plain;
            } else if src.len() < PREAMBLE.len() {
                return Ok(None);
            } else {
                src.advance(PREAMBLE.len());
                self.peer = Peer::Sidecar;
            }
        }

        let mut agent_codec = DaemonCodec::default();

        let client_message = match (self.peer, src.first()) {
            (Peer::Plain, _) => agent_codec.decode(src)?,
            (_, None) => None,
            (_, Some(&AGENT_FRAME)) => {
                // The tag stays in the buffer until its whole payload has arrived.
                let mut payload = src.split_off(1);
                let decoded = agent_codec.decode(&mut payload);
                if matches!(decoded, Ok(Some(_))) {
                    src.clear();
                }
                src.unsplit(payload);
                decoded?
            }
            (_, Some(tag)) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown frame tag {tag} from the internal proxy"),
                ));
            }
        };

        Ok(client_message.map(SidecarMessage::Agent))
    }
}

impl Encoder<DaemonMessage> for ExternalProxyCodec {
    type Error = io::Error;

    fn encode(&mut self, message: DaemonMessage, dst: &mut BytesMut) -> io::Result<()> {
        DaemonCodec::default().encode(message, dst)
    }
}

#[cfg(test)]
mod tests {
    use actix_codec::{Decoder, Encoder};
    use bytes::BytesMut;
    use mirrord_protocol::{ClientCodec, ClientMessage, GetEnvVarsRequest, VERSION};

    use super::{ExternalProxyCodec, InternalProxyCodec, PREAMBLE, SidecarMessage};

    fn client_messages() -> Vec<ClientMessage> {
        vec![
            ClientMessage::SwitchProtocolVersion(VERSION.clone()),
            ClientMessage::Ping,
            ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                env_vars_filter: ["A".to_owned(), "B".to_owned()].into(),
                env_vars_select: ["*".to_owned()].into(),
            }),
            ClientMessage::Close,
        ]
    }

    fn encode_for_agent(message: ClientMessage) -> BytesMut {
        let mut encoded = BytesMut::new();
        ClientCodec::default()
            .encode(message, &mut encoded)
            .unwrap();
        encoded
    }

    /// Decodes `stream` as it arrives when split into chunks of `chunk_size` bytes.
    fn decode_in_chunks(stream: &[u8], chunk_size: usize) -> Vec<SidecarMessage> {
        let mut codec = ExternalProxyCodec::default();
        let mut buffer = BytesMut::new();
        let mut decoded = Vec::new();

        for chunk in stream.chunks(chunk_size) {
            buffer.extend_from_slice(chunk);
            while let Some(message) = codec.decode(&mut buffer).unwrap() {
                decoded.push(message);
            }
        }

        assert!(buffer.is_empty(), "undecoded bytes left: {buffer:?}");
        decoded
    }

    fn expected() -> Vec<SidecarMessage> {
        client_messages()
            .into_iter()
            .map(SidecarMessage::Agent)
            .collect()
    }

    #[test]
    fn external_proxy_decodes_sidecar_protocol() {
        let mut codec = InternalProxyCodec::default();
        let mut stream = BytesMut::new();
        for message in client_messages() {
            codec
                .encode(encode_for_agent(message).to_vec(), &mut stream)
                .unwrap();
        }

        assert!(stream.starts_with(&PREAMBLE));
        for chunk_size in [1, 2, 3, 7, PREAMBLE.len(), stream.len()] {
            assert_eq!(decode_in_chunks(&stream, chunk_size), expected());
        }
    }

    #[test]
    fn external_proxy_decodes_plain_protocol() {
        let mut stream = BytesMut::new();
        for message in client_messages() {
            stream.unsplit(encode_for_agent(message));
        }

        for chunk_size in [1, 2, 3, 7, stream.len()] {
            assert_eq!(decode_in_chunks(&stream, chunk_size), expected());
        }
    }

    #[test]
    fn external_proxy_rejects_unknown_frame() {
        let mut buffer = BytesMut::from(&PREAMBLE[..]);
        buffer.extend_from_slice(&[200, 1, 2, 3]);

        assert!(ExternalProxyCodec::default().decode(&mut buffer).is_err());
    }
}
