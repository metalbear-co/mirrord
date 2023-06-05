use std::{
    env,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::RangeInclusive,
    str::FromStr,
};

use tracing::{error, warn};

/// Environment variable used to tell the layer that it should dynamically detect the local
/// port used by the given debugger. Value passed through this variable should parse into
/// [`DebuggerType`]. Used when injecting the layer through IDE, because the debugger port is chosen
/// dynamically.
pub const MIRRORD_DETECT_DEBUGGER_PORT_ENV: &str = "MIRRORD_DETECT_DEBUGGER_PORT";

/// Environment variable used to tell the layer that it should ignore certain local ports that may
/// be used by the debugger. Used when injecting the layer through IDE. This setting will be ignored
/// if the layer successfully detects the port at runtime, see [`MIRRORD_DETECT_DEBUGGER_PORT_ENV`].
///
/// Value passed through this variable can represent a single port like '12233' or a range of ports
/// like `12233-13000`.
pub const MIRRORD_IGNORE_DEBUGGER_PORTS_ENV: &str = "MIRRORD_IGNORE_DEBUGGER_PORTS";

/// Type of debugger which is used to run the user's processes.
/// Determines the way we parse the command line arguments the debugger's port.
/// Logic of processing the arguments is based on examples taken from the IDEs.
#[derive(Debug, Clone, Copy)]
pub enum DebuggerType {
    /// An implementation of the [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/) for Python 3.
    /// Used in VS Code.
    ///
    /// Command used to invoke this debugger looked like
    /// `/path/to/python /path/to/vscode/extensions/debugpy --connect 127.0.0.1:57141
    /// --configure-qt none --adapter-access-token
    /// c2d745556a5a571d09dbf9c14af2898b3d6c174597d6b7198d9d30c105d5ab24 /path/to/script.py`
    ///
    /// Port would not be extracted from a command like `/path/to/python /path/to/script.py ...`
    /// (debugger name missing) or `/path/to/python /path/to/vscode/extensions/debugpy
    /// /path/to/script.py` (socket missing).
    DebugPy,
    /// Used in PyCharm.
    ///
    /// Command used to invoke this debugger looked like
    /// `/path/to/python /path/to/pycharm/plugins/pydevd.py --multiprocess --qt-support=auto
    /// --client 127.0.0.1 --port 32845 --file /path/to/script.py`
    ///
    /// Port would not be extracted from a command like `/path/to/python /path/to/script.py ...`
    /// (debugger name missing) or `/path/to/pycharm/plugins/pydevd.py ...` (python invokation
    /// missing) or `/path/to/python /path/to/pycharm/plugins/pydevd.py --client 127.0.0.1 ...`
    /// (port missing).
    PyDevD,
    /// Used in Rider.
    ///
    /// Command used to invoke this debugger looked like
    /// `/path/to/rider/lib/ReSharperHost/linux-x64/dotnet/dotnet exec
    /// /path/to/rider/lib/ReSharperHost/JetBrains.Debugger.Worker.exe --mode=client
    /// --frontend-port=36977 ...`.
    ///
    /// Port would not be extractged from a command like
    /// `/some/executable exec /path/to/rider/lib/ReSharperHost/JetBrains.Debugger.Worker.exe
    /// --mode=client --frontend-port=36977 ...` (dotnet executable missing) or
    /// `/path/to/rider/lib/ ReSharperHost/linux-x64/dotnet/dotnet exec --mode=client
    /// --frontend-port=36977 ...` (debugger missing) or
    /// `/path/to/rider/lib/ReSharperHost/linux-x64/dotnet/dotnet exec /path/to/rider/lib/
    /// ReSharperHost/JetBrains.Debugger.Worker.exe --mode=client ...` (port missing).
    ReSharper,
}

impl FromStr for DebuggerType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "debugpy" => Ok(Self::DebugPy),
            "pydevd" => Ok(Self::PyDevD),
            "resharper" => Ok(Self::ReSharper),
            _ => Err(format!("invalid debugger type: {s}")),
        }
    }
}

impl DebuggerType {
    /// Retrieves the port used by debugger of this type from the command.
    fn get_port(self, args: &[String]) -> Option<u16> {
        match self {
            Self::DebugPy => {
                let is_python = args.first()?.rsplit('/').next()?.starts_with("py");
                let runs_debugpy = args.get(1)?.ends_with("debugpy");

                if !is_python || !runs_debugpy {
                    None?
                }

                args.windows(2).find_map(|window| match window {
                    [opt, val] if opt == "--connect" => val.parse::<SocketAddr>().ok(),
                    _ => None,
                })
            }
            Self::PyDevD => {
                let is_python = args.first()?.rsplit('/').next()?.starts_with("py");
                let runs_pydevd = args.get(1)?.rsplit('/').next()?.contains("pydevd");

                if !is_python || !runs_pydevd {
                    None?
                }

                let client = args.windows(2).find_map(|window| match window {
                    [opt, val] if opt == "--client" => val.parse::<IpAddr>().ok(),
                    _ => None,
                })?;
                let port = args.windows(2).find_map(|window| match window {
                    [opt, val] if opt == "--port" => val.parse::<u16>().ok(),
                    _ => None,
                })?;

                SocketAddr::new(client, port).into()
            }
            Self::ReSharper => {
                let is_dotnet = args.first()?.ends_with("dotnet");
                let runs_debugger = args.get(2)?.contains("Debugger");

                if !is_dotnet || !runs_debugger {
                    None?
                }

                args.iter()
                    .find_map(|arg| arg.strip_prefix("--frontend-port="))
                    .and_then(|port| port.parse::<u16>().ok())
                    .map(|port| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
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
pub enum DebuggerPorts {
    Detected(u16),
    FixedRange(RangeInclusive<u16>),
    None,
}

impl DebuggerPorts {
    /// Create a new instance of this enum based on the environment variables
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
        if let Some(port) = detected {
            return Self::Detected(port);
        }

        let fixed_range = env::var(MIRRORD_IGNORE_DEBUGGER_PORTS_ENV)
            .ok()
            .and_then(|s| {
                let chunks = s
                    .split('-')
                    .map(u16::from_str)
                    .collect::<Result<Vec<_>, _>>()
                    .inspect_err(|e| error!(
                        "Failed to decode debugger ports from {} env variable: {}",
                        MIRRORD_IGNORE_DEBUGGER_PORTS_ENV,
                        e
                    ))
                    .ok()?;
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
        if let Some(range) = fixed_range {
            return Self::FixedRange(range);
        }

        Self::None
    }

    /// Return whether the given [SocketAddr] is used by the debugger.
    pub fn contains(&self, addr: &SocketAddr) -> bool {
        let is_localhost = matches!(
            addr.ip(),
            IpAddr::V4(Ipv4Addr::LOCALHOST) | IpAddr::V6(Ipv6Addr::LOCALHOST)
        );
        if !is_localhost {
            return false;
        }

        match self {
            Self::Detected(port) => *port == addr.port(),
            Self::FixedRange(range) => range.contains(&addr.port()),
            Self::None => false,
        }
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
            debugger.get_port(
                &command
                    .split_ascii_whitespace()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            ),
            Some(57141),
        )
    }

    #[test]
    fn detect_pydevd_port() {
        let debugger = DebuggerType::PyDevD;
        let command = "/path/to/python /path/to/pycharm/plugins/pydevd.py --multiprocess --qt-support=auto --client 127.0.0.1 --port 32845 --file /path/to/script.py";

        assert_eq!(
            debugger.get_port(
                &command
                    .split_ascii_whitespace()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            ),
            Some(32845),
        )
    }

    #[test]
    fn detect_resharper_port() {
        let debugger = DebuggerType::ReSharper;
        let command = "/path/to/rider/lib/ReSharperHost/linux-x64/dotnet/dotnet exec /path/to/rider/lib/ReSharperHost/JetBrains.Debugger.Worker.exe --mode=client --frontend-port=40905 --plugins=/path/to/rider/plugins/rider-unity/dotnetDebuggerWorker;/path/to/rider/plugins/dpa/DotFiles/JetBrains.DPA.DebugInjector.dll --etw-collect-flags=2 --backend-pid=222222 --handle=333";

        assert_eq!(
            debugger.get_port(
                &command
                    .split_ascii_whitespace()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            ),
            Some(40905)
        )
    }

    #[test]
    fn debugger_ports_contain() {
        assert!(DebuggerPorts::Detected(1337).contains(&"127.0.0.1:1337".parse().unwrap()));
        assert!(!DebuggerPorts::Detected(1337).contains(&"127.0.0.1:1338".parse().unwrap()));
        assert!(!DebuggerPorts::Detected(1337).contains(&"8.8.8.8:1337".parse().unwrap()));

        assert!(
            DebuggerPorts::FixedRange(45000..=50000).contains(&"127.0.0.1:47888".parse().unwrap())
        );
        assert!(
            !DebuggerPorts::FixedRange(45000..=50000).contains(&"127.0.0.1:80".parse().unwrap())
        );
        assert!(
            !DebuggerPorts::FixedRange(45000..=50000).contains(&"8.8.8.8:47888".parse().unwrap())
        );

        assert!(!DebuggerPorts::None.contains(&"127.0.0.1:1337".parse().unwrap()));
    }
}
