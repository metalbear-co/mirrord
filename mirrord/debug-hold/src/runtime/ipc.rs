//! The few frames of the .NET diagnostics IPC protocol the runtime pause needs: the advertise a
//! runtime sends when it connects, ProcessInfo3 with its two older forms, and ResumeRuntime.
//!
//! The protocol is dotnet/diagnostics' `documentation/design-docs/ipc-protocol.md`. Every integer
//! is little endian. A string is a `u32` count of UTF-16 code units, the terminating NUL included,
//! then the units.
//!
//! A frame is a 20-byte header, then its payload. The header is the magic `DOTNET_IPC_V1\0` (14
//! bytes), the whole frame's size (`u16`), the command set, the command id, and two reserved bytes.
//! Every response is in the Server command set: OK, or an error whose payload is an HRESULT.

use std::{io, time::Duration};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

/// The size of the advertise: its magic, the runtime's cookie, its pid, and two reserved bytes.
pub(super) const ADVERTISE_SIZE: usize = 34;

const ADVERTISE_MAGIC: &[u8; 8] = b"ADVR_V1\0";

const FRAME_MAGIC: &[u8; 14] = b"DOTNET_IPC_V1\0";

const HEADER_SIZE: usize = 20;

/// The Process command set.
const PROCESS: u8 = 0x04;

/// The Server command set, which every response is in.
const SERVER: u8 = 0xFF;

const OK: u8 = 0x00;

/// `DS_IPC_E_UNKNOWN_COMMAND`, the HRESULT a runtime answers a command it doesn't know with, as
/// one older than .NET 8 answers ProcessInfo3. No crate defines it; dotnet/runtime does, in
/// [`ds-types.h`](https://github.com/dotnet/runtime/blob/feda17e1e6a4f90aeb7285d1d8528342799515c9/src/native/eventpipe/ds-types.h#L129).
#[cfg(test)]
pub(super) const DS_IPC_E_UNKNOWN_COMMAND: u32 = 0x8013_1385;

/// What a runtime says when it connects, before any command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Advertise {
    /// The runtime instance, new in every process. The runtime re-sends it on every connection,
    /// so it ties a runtime's connections together.
    pub(super) cookie: Uuid,
    /// The process the runtime says it is, which the connection's peer must be.
    pub(super) pid: u32,
}

/// A request a paused runtime is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Command {
    /// The pid, cookie, command line, OS and architecture: every runtime with diagnostic ports.
    ProcessInfo,
    /// ProcessInfo plus the entry assembly and the runtime's version, from .NET 6.
    ProcessInfo2,
    /// A version, ProcessInfo2's fields and the runtime identifier, from .NET 8.
    ProcessInfo3,
    /// Lets a runtime paused at its startup go on.
    ResumeRuntime,
}

impl Command {
    fn id(self) -> u8 {
        match self {
            Self::ProcessInfo => 0x00,
            Self::ResumeRuntime => 0x01,
            Self::ProcessInfo2 => 0x04,
            Self::ProcessInfo3 => 0x08,
        }
    }

    /// The request's frame: a header with no payload.
    pub(super) fn frame(self) -> [u8; HEADER_SIZE] {
        let mut frame = [0; HEADER_SIZE];
        let (magic, rest) = frame.split_at_mut(FRAME_MAGIC.len());
        magic.copy_from_slice(FRAME_MAGIC);
        let (size, rest) = rest.split_at_mut(2);
        size.copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
        if let [set, id, ..] = rest {
            *set = PROCESS;
            *id = self.id();
        }
        frame
    }
}

/// What a runtime says about itself in a ProcessInfo reply. Fields a form doesn't have are `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ProcessInfo {
    pub(super) command_line: Option<String>,
    pub(super) os: Option<String>,
    pub(super) arch: Option<String>,
    /// The managed entry assembly's name, without `.dll`: the app's own, `dotnet` for the SDK.
    /// Known at the startup pause, before the assembly loads.
    pub(super) entry_assembly: Option<String>,
    pub(super) runtime_version: Option<String>,
    pub(super) rid: Option<String>,
}

/// Why a frame couldn't be read, or what the runtime answered instead of OK.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("failed to read or write the diagnostic connection")]
    Io(#[from] io::Error),

    #[error("the runtime didn't answer within {0:?}")]
    TimedOut(Duration),

    #[error("the connection ended after {read} of {expected} bytes")]
    Ended { read: usize, expected: usize },

    #[error("the frame doesn't start with its protocol's magic")]
    BadMagic,

    #[error("the frame says it is {0} bytes, less than its own header")]
    BadSize(u16),

    #[error("the advertise names pid {0}, which no process can have")]
    BadPid(u64),

    #[error("the runtime answered with error {hresult:#010x}")]
    Refused { hresult: u32 },

    #[error("the reply's payload ends before its fields do")]
    Truncated,
}

/// Reads a runtime's advertise: the 34 bytes it sends first on every connection.
pub(super) fn parse_advertise(frame: &[u8; ADVERTISE_SIZE]) -> Result<Advertise, IpcError> {
    let mut reader = Reader(frame);
    if reader.take(ADVERTISE_MAGIC.len())? != ADVERTISE_MAGIC {
        return Err(IpcError::BadMagic);
    }
    let cookie = reader.take(16)?;
    let cookie = Uuid::from_slice_le(cookie).map_err(|_| IpcError::Truncated)?;
    let pid = reader.u64()?;
    let pid = u32::try_from(pid)
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or(IpcError::BadPid(pid))?;
    Ok(Advertise { cookie, pid })
}

/// Reads one response frame, and its payload when it's OK.
pub(super) async fn read_response(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<Vec<u8>, IpcError> {
    let mut header = [0; HEADER_SIZE];
    read_exactly(stream, &mut header).await?;
    let (magic, rest) = header.split_at(FRAME_MAGIC.len());
    if magic != FRAME_MAGIC {
        return Err(IpcError::BadMagic);
    }
    let [size_low, size_high, set, id, ..] = *rest else {
        return Err(IpcError::Truncated);
    };
    let size = u16::from_le_bytes([size_low, size_high]);
    let payload_size = usize::from(size)
        .checked_sub(HEADER_SIZE)
        .ok_or(IpcError::BadSize(size))?;
    let mut payload = vec![0; payload_size];
    read_exactly(stream, &mut payload).await?;

    match (set, id) {
        (SERVER, OK) => Ok(payload),
        // An error's payload is its HRESULT; one too short to hold it, or any other answer, is
        // still an error.
        _ => Err(IpcError::Refused {
            hresult: Reader(&payload).u32().unwrap_or_default(),
        }),
    }
}

/// Sends `command` and reads its answer, within `timeout`.
pub(super) async fn request(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    command: Command,
    timeout: Duration,
) -> Result<Vec<u8>, IpcError> {
    tokio::time::timeout(timeout, async {
        stream.write_all(&command.frame()).await?;
        stream.flush().await?;
        read_response(stream).await
    })
    .await
    .map_err(|_| IpcError::TimedOut(timeout))?
}

/// Reads a ProcessInfo reply's payload, in the form `command` asked for.
pub(super) fn parse_process_info(
    command: Command,
    payload: &[u8],
) -> Result<ProcessInfo, IpcError> {
    let mut reader = Reader(payload);
    if command == Command::ProcessInfo3 {
        let _version = reader.u32()?;
    }
    let _pid = reader.u64()?;
    let _cookie = reader.take(16)?;
    let mut info = ProcessInfo {
        command_line: Some(reader.string()?),
        os: Some(reader.string()?),
        arch: Some(reader.string()?),
        ..ProcessInfo::default()
    };
    if command != Command::ProcessInfo {
        info.entry_assembly = Some(reader.string()?);
        info.runtime_version = Some(reader.string()?);
    }
    // Version 0 of ProcessInfo3 may end before its runtime identifier.
    if command == Command::ProcessInfo3 && !reader.0.is_empty() {
        info.rid = Some(reader.string()?);
    }
    Ok(info)
}

/// Fills `buffer`, or says how far the connection got before it ended.
pub(super) async fn read_exactly(
    stream: &mut (impl AsyncRead + Unpin),
    buffer: &mut [u8],
) -> Result<(), IpcError> {
    let mut read = 0;
    while let Some(rest) = buffer.get_mut(read..).filter(|rest| !rest.is_empty()) {
        match stream.read(rest).await? {
            0 => {
                return Err(IpcError::Ended {
                    read,
                    expected: buffer.len(),
                });
            }
            got => read += got,
        }
    }
    Ok(())
}

/// A cursor over a payload, failing with [`IpcError::Truncated`] past its end.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], IpcError> {
        if self.0.len() < count {
            return Err(IpcError::Truncated);
        }
        let (taken, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(taken)
    }

    fn u32(&mut self) -> Result<u32, IpcError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(|_| IpcError::Truncated)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, IpcError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(
            bytes.try_into().map_err(|_| IpcError::Truncated)?,
        ))
    }

    /// A string of UTF-16 code units, without its terminating NUL.
    fn string(&mut self) -> Result<String, IpcError> {
        let units = usize::try_from(self.u32()?).map_err(|_| IpcError::Truncated)?;
        let bytes = self.take(units.checked_mul(2).ok_or(IpcError::Truncated)?)?;
        let (units, _) = bytes.as_chunks::<2>();
        let units = units
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .collect::<Vec<_>>();
        Ok(String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes a recorded frame holds, from its `.hex` file in `testdata`.
    fn recorded(hex: &str) -> Vec<u8> {
        let hex = hex.trim();
        (0..hex.len())
            .step_by(2)
            .map(|at| {
                let digits = hex.get(at..at + 2).expect("whole bytes");
                u8::from_str_radix(digits, 16).expect("hex")
            })
            .collect()
    }

    /// The cookie and pid a runtime advertised in the recorded handshake.
    const RECORDED_COOKIE: &str = "6ea9bcc5-d70f-43e4-a5a5-abafb0c22dc7";
    const RECORDED_PID: u32 = 30508;

    /// A real .NET 8 runtime's advertise: the cookie reads the way .NET prints its GUIDs, so the
    /// IDE and `dotnet-trace` name the same runtime.
    #[test]
    fn a_recorded_advertise_names_its_runtime_and_pid() {
        let frame: [u8; ADVERTISE_SIZE] = recorded(include_str!("testdata/advertise.hex"))
            .try_into()
            .expect("an advertise is 34 bytes");

        let advertise = parse_advertise(&frame).expect("a real runtime's advertise");

        assert_eq!(advertise.cookie.to_string(), RECORDED_COOKIE);
        assert_eq!(advertise.pid, RECORDED_PID);
    }

    /// Another protocol version's advertise is refused, rather than read as this one.
    #[test]
    fn an_advertise_of_another_version_is_refused() {
        let frame: [u8; ADVERTISE_SIZE] =
            recorded(include_str!("testdata/advertise-bad-magic.hex"))
                .try_into()
                .expect("an advertise is 34 bytes");

        assert!(matches!(parse_advertise(&frame), Err(IpcError::BadMagic)));
    }

    /// A real .NET 8 runtime's ProcessInfo3 reply: the entry assembly is what the SDK tooling
    /// skip goes by, and the runtime identifier comes last.
    #[tokio::test]
    async fn a_recorded_process_info3_reply_names_the_entry_assembly() {
        let reply = recorded(include_str!("testdata/processinfo3.hex"));

        let payload = read_response(&mut reply.as_slice())
            .await
            .expect("an OK reply");
        let info = parse_process_info(Command::ProcessInfo3, &payload).expect("its fields");

        assert_eq!(
            info,
            ProcessInfo {
                command_line: Some(r#"".\FakeUp.exe" --idle"#.to_owned()),
                os: Some("Windows".to_owned()),
                arch: Some("x64".to_owned()),
                entry_assembly: Some("FakeUp".to_owned()),
                runtime_version: Some("8.0.31".to_owned()),
                rid: Some("win-x64".to_owned()),
            }
        );
    }

    /// ProcessInfo2 has no version in front: read as ProcessInfo3, its fields would shift.
    #[tokio::test]
    async fn a_recorded_process_info2_reply_has_no_version_in_front() {
        let reply = recorded(include_str!("testdata/processinfo2.hex"));

        let payload = read_response(&mut reply.as_slice())
            .await
            .expect("an OK reply");
        let info = parse_process_info(Command::ProcessInfo2, &payload).expect("its fields");

        assert_eq!(info.entry_assembly.as_deref(), Some("FakeUp"));
        assert_eq!(info.rid, None, "only ProcessInfo3 has a runtime identifier");
    }

    /// A reply whose connection ends inside it is an error, never a reply with fields missing.
    #[tokio::test]
    async fn a_truncated_reply_is_an_error() {
        let reply = recorded(include_str!("testdata/processinfo3-truncated.hex"));

        let read = read_response(&mut reply.as_slice()).await;

        assert!(
            matches!(read, Err(IpcError::Ended { .. })),
            "the frame says it's longer than what arrived: {read:?}"
        );
    }

    /// A string whose length runs past its payload is refused, not read out of bounds.
    #[test]
    fn a_string_longer_than_its_payload_is_truncated() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u64.to_le_bytes());
        payload.extend_from_slice(&[0; 16]);
        payload.extend_from_slice(&u32::MAX.to_le_bytes());

        assert!(matches!(
            parse_process_info(Command::ProcessInfo, &payload),
            Err(IpcError::Truncated)
        ));
    }

    /// The recorded ResumeRuntime reply is OK, and the request is the header the runtime expects.
    #[tokio::test]
    async fn resume_runtime_is_a_bare_header_and_its_reply_is_ok() {
        assert_eq!(
            Command::ResumeRuntime.frame().as_slice(),
            recorded(
                "444F544E45545F4950435F5631001400040100 00"
                    .replace(' ', "")
                    .as_str()
            ),
            "magic, size 20, the Process set, ResumeRuntime, reserved"
        );
        let reply = recorded(include_str!("testdata/resume.hex"));
        assert!(read_response(&mut reply.as_slice()).await.is_ok());
    }

    /// An error reply's HRESULT reaches the caller, which reports it.
    #[tokio::test]
    async fn an_error_reply_carries_its_hresult() {
        let mut reply = Vec::new();
        reply.extend_from_slice(FRAME_MAGIC);
        reply.extend_from_slice(&24u16.to_le_bytes());
        // The Server set's error.
        reply.extend_from_slice(&[SERVER, 0xFF, 0, 0]);
        reply.extend_from_slice(&DS_IPC_E_UNKNOWN_COMMAND.to_le_bytes());

        assert!(matches!(
            read_response(&mut reply.as_slice()).await,
            Err(IpcError::Refused {
                hresult: DS_IPC_E_UNKNOWN_COMMAND
            })
        ));
    }
}
