use std::{
    env,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::RangeInclusive,
    str::FromStr,
};

use tracing::{error, warn};

/// Environment variable used to tell the layer that it should dynamically detect the local port
/// port used by the given debugger. Value passed through this variable should parse into
/// [`DebuggerType`]. Used when injecting the layer through IDE, because the debugger port is chosen
/// dynamically.
pub const MIRRORD_DETECT_DEBUGGER_PORT_ENV: &str = "MIRRORD_DETECT_DEBUGGER_PORT";

/// Environment variable used to tell the layer that it should ignore certain local ports used by
/// the debugger. Used when injecting the layer through IDE.
///
/// Value passed through this variable can represent a single port like '12233' or a range of ports
/// like `12233-13000`.
pub const MIRRORD_IGNORE_DEBUGGER_PORTS_ENV: &str = "MIRRORD_IGNORE_DEBUGGER_PORTS";

/// Type of debugger which is used to run the user's processes.
#[derive(Debug, Clone, Copy)]
pub enum DebuggerType {
    DebugPy,
}

impl FromStr for DebuggerType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "debugpy" => Ok(Self::DebugPy),
            _ => Err(format!("invalid debugger type: {s}")),
        }
    }
}

impl DebuggerType {
    fn get_port(self, args: &[String]) -> Option<u16> {
        match self {
            Self::DebugPy => {
                let is_python = args.first()?.rsplit('/').next().unwrap().contains("python");
                let runs_debugpy = args.get(1)?.ends_with("debugpy");
                if is_python && runs_debugpy {
                    args.windows(2).find_map(|window| match window {
                        [opt, val] if opt == "--connect" => val.parse::<SocketAddr>().ok(),
                        _ => None,
                    })
                } else {
                    None
                }
            }
        }
        .and_then(|addr| match addr.ip() {
            IpAddr::V4(Ipv4Addr::LOCALHOST) | IpAddr::V6(Ipv6Addr::LOCALHOST) => Some(addr.port()),
            other => {
                warn!(
                    "Debugger uses a remote socket address {:?}! This case is not yet handled properly.",
                    other,
                );
                None
            }
        })
    }
}

/// Local ports used by the debugger running the process.
/// These should be ignored by the layer.
#[derive(Debug)]
pub struct DebuggerPorts {
    detected: Option<u16>,
    fixed: Option<RangeInclusive<u16>>,
}

impl DebuggerPorts {
    /// Create a new instance of this struct based on the environment variables
    /// ([`MIRRORD_DETECT_DEBUGGER_PORT_ENV`] and [`MIRRORD_IGNORE_DEBUGGER_PORTS_ENV`]) and command
    /// line arguments.
    ///
    /// Log errors (like malformed env variables) but do not panic.
    pub fn from_env() -> Self {
        let detected = env::var(MIRRORD_DETECT_DEBUGGER_PORT_ENV)
            .ok()
            .and_then(|s| {
                DebuggerType::from_str(&s)
                    .inspect_err(|e| {
                        error!(
                            "Failed to decode debugger type from {} env variable: {}",
                            MIRRORD_DETECT_DEBUGGER_PORT_ENV, e
                        )
                    })
                    .ok()
            })
            .and_then(|d| d.get_port(&std::env::args().collect::<Vec<_>>()));

        let fixed = env::var(MIRRORD_IGNORE_DEBUGGER_PORTS_ENV).ok().and_then(|s| {
            let chunks = s.split('-').map(u16::from_str).collect::<Result<Vec<_>, _>>().inspect_err(|e| error!("Failed to decode debugger ports from {} env variable: {}", MIRRORD_IGNORE_DEBUGGER_PORTS_ENV, e)).ok()?;
            match *chunks.as_slice() {
                [p] => Some(p..=p),
                [p1, p2] if p1 <= p2 => Some(p1..=p2),
                _ => {
                    error!(
                        "Failed to decode debugger ports from {} env variable: expected a port or a range of ports",
                        MIRRORD_IGNORE_DEBUGGER_PORTS_ENV,
                    );
                    None
                },
            }
        });

        Self { detected, fixed }
    }

    /// Return whether the given [SocketAddr] is used by the debugger.
    pub fn contains(&self, addr: &SocketAddr) -> bool {
        matches!(
            addr.ip(),
            IpAddr::V4(Ipv4Addr::LOCALHOST) | IpAddr::V6(Ipv6Addr::LOCALHOST)
        ) && (self.detected == Some(addr.port())
            || self
                .fixed
                .as_ref()
                .map(|r| r.contains(&addr.port()))
                .unwrap_or(false))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn detect_debugpy_port() {
        let debugger = DebuggerType::DebugPy;
        let command = "/home/user/path/to/venv/bin/python /home/user/.vscode/extensions/ms-python.python-2023.6.1/pythonFiles/lib/python/debugpy/adapter/../../debugpy/launcher/../../debugpy --connect 127.0.0.1:57141 --configure-qt none --adapter-access-token c2d745556a5a571d09dbf9c14af2898b3d6c174597d6b7198d9d30c105d5ab24 /home/user/path/to/script.py";

        assert_eq!(
            debugger.get_port(&command.split_ascii_whitespace().map(ToString::to_string).collect::<Vec<_>>()),
            Some(57141),
        )
    }
}
