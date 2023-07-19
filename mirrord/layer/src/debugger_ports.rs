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
///
/// When a debugger port is detected this way, the layer removes this variable and sets
/// [`MIRRORD_IGNORE_DEBUGGER_PORTS_ENV`] for child processes.
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
    /// Port would not be extracted from a command like
    /// `/some/executable exec /path/to/rider/lib/ReSharperHost/JetBrains.Debugger.Worker.exe
    /// --mode=client --frontend-port=36977 ...` (dotnet executable missing) or
    /// `/path/to/rider/lib/ ReSharperHost/linux-x64/dotnet/dotnet exec --mode=client
    /// --frontend-port=36977 ...` (debugger missing) or
    /// `/path/to/rider/lib/ReSharperHost/linux-x64/dotnet/dotnet exec /path/to/rider/lib/
    /// ReSharperHost/JetBrains.Debugger.Worker.exe --mode=client ...` (port missing).
    ReSharper,
    /// Used in both VSCode and IDEA when debugging java applications.
    ///
    /// Command on VSCode and macOS looks like
    /// /Users/meee/Library/Java/JavaVirtualMachines/corretto-17.0.4.1/Contents/Home/bin/java
    /// -agentlib:jdwp=transport=dt_socket,server=n,susp end=y,address=localhost:54898
    /// @/var/folders/2h/fn_s1t8n0cqfc9x71yq845m40000gn/T/cp_dikq30ybalqwcehe333w2xxhd.argfile
    /// com.example.demo.DemoApplication
    JavaAgent,
}

impl FromStr for DebuggerType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "debugpy" => Ok(Self::DebugPy),
            "pydevd" => Ok(Self::PyDevD),
            "resharper" => Ok(Self::ReSharper),
            "javaagent" => Ok(Self::JavaAgent),
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
            Self::JavaAgent => {
                let is_java = args.first()?.ends_with("java");

                if !is_java {
                    None?
                }

                args.iter()
                    .find_map(|arg| arg.strip_prefix("-agentlib:jdwp=transport=dt_socket"))
                    .and_then(|agent_lib_args| agent_lib_args.split(',').find_map(|arg| arg.strip_prefix("address=")))
                    .and_then(|full_address| full_address.split(':').last())
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
            env::set_var(MIRRORD_IGNORE_DEBUGGER_PORTS_ENV, port.to_string());
            env::remove_var(MIRRORD_DETECT_DEBUGGER_PORT_ENV);
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
    use rstest::rstest;

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

    #[rstest]
    // macOS + IntelliJ IDEA
    #[case("/Library/Java/JavaVirtualMachines/jdk-18.0.1.1.jdk/Contents/Home/bin/java -agentlib:jdwp=transport=dt_socket,address=127.0.0.1:54898,suspend=y,server=n -XX:TieredStopAtLevel=1 -Dspring.output.ansi.enabled=always -Dcom.sun.management.jmxremote -Dspring.jmx.enabled=true -Dspring.liveBeansView.mbeanDomain -Dspring.application.admin.enabled=true -Dmanagement.endpoints.jmx.exposure.include=* -javaagent:/Users/aviramhassan/Library/Caches/JetBrains/IntelliJIdea2023.1/captureAgent/debugger-agent.jar -Dfile.encoding=UTF-8 -Dsun.stdout.encoding=UTF-8 -Dsun.stderr.encoding=UTF-8 -classpath /Users/aviramhassan/Code/springdemo/demo/build/classes/java/main:/Users/aviramhassan/Code/springdemo/demo/build/resources/main:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework.boot/spring-boot-starter-web/3.0.4/6a7405b436c6943f056cdbab587fe48bdc2b4911/spring-boot-starter-web-3.0.4.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework.boot/spring-boot-starter-json/3.0.4/5ddf427d011646fe787fdaa1b8edc4a4eebd48d5/spring-boot-starter-json-3.0.4.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework.boot/spring-boot-starter/3.0.4/8f957a8bc9d70c0ca922406c6e23cb3db5d48109/spring-boot-starter-3.0.4.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework.boot/spring-boot-starter-tomcat/3.0.4/40babd5ca3db46adf7c59b70ca2e6f2a528b558c/spring-boot-starter-tomcat-3.0.4.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework/spring-webmvc/6.0.6/302580efc981ad6797a85814ea0996e2149bb420/spring-webmvc-6.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework/spring-web/6.0.6/2916961032e54aaeb534a290530b7b69e297bfcc/spring-web-6.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.datatype/jackson-datatype-jsr310/2.14.2/796518148a385b2728d44886cc0f8852eb8eeb53/jackson-datatype-jsr310-2.14.2.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.module/jackson-module-parameter-names/2.14.2/2b6c19b3d99dda02915515df879ab9e23fed3864/jackson-module-parameter-names-2.14.2.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.datatype/jackson-datatype-jdk8/2.14.2/2f3c71211b6ea7a978eba33574d7135d536e07fb/jackson-datatype-jdk8-2.14.2.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.core/jackson-databind/2.14.2/1e71fddbc80bb86f71a6345ac1e8ab8a00e7134/jackson-databind-2.14.2.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework.boot/spring-boot-autoconfigure/3.0.4/7eb11bff0f965807f1088da20bc169bff27d284/spring-boot-autoconfigure-3.0.4.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework.boot/spring-boot/3.0.4/27e5fceb2faf8ec399df70a2ff4e626a3423ae35/spring-boot-3.0.4.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework.boot/spring-boot-starter-logging/3.0.4/31b32774c6ec2eceb3f75bdef7ddd8afd7059255/spring-boot-starter-logging-3.0.4.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/jakarta.annotation/jakarta.annotation-api/2.1.1/48b9bda22b091b1f48b13af03fe36db3be6e1ae3/jakarta.annotation-api-2.1.1.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework/spring-core/6.0.6/8a2845e0945923a9ebf3e9ef0649a28b6eeeac43/spring-core-6.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.yaml/snakeyaml/1.33/2cd0a87ff7df953f810c344bdf2fe3340b954c69/snakeyaml-1.33.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.apache.tomcat.embed/tomcat-embed-websocket/10.1.5/14529cbd593571dc9029272ddc9166b5ef113fc2/tomcat-embed-websocket-10.1.5.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.apache.tomcat.embed/tomcat-embed-core/10.1.5/21417d3ef8189e2af05aae0a765ad9204d7211b5/tomcat-embed-core-10.1.5.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.apache.tomcat.embed/tomcat-embed-el/10.1.5/c125df13af42a0fc0cd342370449b1276181e2a1/tomcat-embed-el-10.1.5.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework/spring-context/6.0.6/fbd2b7c23adb2ec2f7ca601b3e7d79ae10e342a/spring-context-6.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework/spring-aop/6.0.6/c95dc800fdce470519b7135272e64d4606fc8428/spring-aop-6.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework/spring-beans/6.0.6/48ea4ba141146b3acaad0c7df80d2a06afeb95fd/spring-beans-6.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework/spring-expression/6.0.6/2006ee0e1be8380f05c29deb52a97d3a1e6812d7/spring-expression-6.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/io.micrometer/micrometer-observation/1.10.4/96aa92ad7a18c2451c429348c0167eb08a988d4e/micrometer-observation-1.10.4.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.core/jackson-annotations/2.14.2/a7aae9525864930723e3453ab799521fdfd9d873/jackson-annotations-2.14.2.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.core/jackson-core/2.14.2/f804090e6399ce0cf78242db086017512dd71fcc/jackson-core-2.14.2.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/ch.qos.logback/logback-classic/1.4.5/28e7dc0b208d6c3f15beefd73976e064b4ecfa9b/logback-classic-1.4.5.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.apache.logging.log4j/log4j-to-slf4j/2.19.0/30f4812e43172ecca5041da2cb6b965cc4777c19/log4j-to-slf4j-2.19.0.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.slf4j/jul-to-slf4j/2.0.6/c4d348977a83a0bfcf42fd6fd1fee6e7904f1a0c/jul-to-slf4j-2.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.springframework/spring-jcl/6.0.6/f13cd8a561e71fb65c63201113d766fcd7a2b06f/spring-jcl-6.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/io.micrometer/micrometer-commons/1.10.4/a5db33c573c8755e70388f204e83000dc359b63d/micrometer-commons-1.10.4.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/ch.qos.logback/logback-core/1.4.5/e9bb2ea70f84401314da4300343b0a246c8954da/logback-core-1.4.5.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.slf4j/slf4j-api/2.0.6/88c40d8b4f33326f19a7d3c0aaf2c7e8721d4953/slf4j-api-2.0.6.jar:/Users/aviramhassan/.gradle/caches/modules-2/files-2.1/org.apache.logging.log4j/log4j-api/2.19.0/ea1b37f38c327596b216542bc636cfdc0b8036fa/log4j-api-2.19.0.jar:/Applications/IntelliJ IDEA.app/Contents/lib/idea_rt.jar com.example.demo.DemoApplication")]
    // macOS + VSCode
    #[case("/Users/me/Library/Java/JavaVirtualMachines/corretto-17.0.4.1/Contents/Home/bin/java -agentlib:jdwp=transport=dt_socket,server=n,suspend=y,address=localhost:54898 @/var/folders/2h/fn_s1t8n0cqfc9x71yq845m40000gn/T/cp_dikq30ybalqwcehe333w2xxhd.argfile com.example.demo.DemoApplication")]
    // Linux + IntelliJ IDEA
    #[case("/home/woot/.jdks/corretto-17.0.7/bin/java -agentlib:jdwp=transport=dt_socket,server=n,suspend=y,address=127.0.0.1:54898 -XX:TieredStopAtLevel=1 -Dfile.encoding=UTF-8 -Duser.country=US -Duser.language=en -Duser.variant -cp /home/woot/Downloads/demo/demo/build/classes/java/main:/home/woot/Downloads/demo/demo/build/resources/main:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework/spring-webmvc/6.0.6/302580efc981ad6797a85814ea0996e2149bb420/spring-webmvc-6.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework/spring-web/6.0.6/2916961032e54aaeb534a290530b7b69e297bfcc/spring-web-6.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework.boot/spring-boot-autoconfigure/3.0.4/7eb11bff0f965807f1088da20bc169bff27d284/spring-boot-autoconfigure-3.0.4.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework.boot/spring-boot/3.0.4/27e5fceb2faf8ec399df70a2ff4e626a3423ae35/spring-boot-3.0.4.jar:/home/woot/.gradle/caches/modules-2/files-2.1/jakarta.annotation/jakarta.annotation-api/2.1.1/48b9bda22b091b1f48b13af03fe36db3be6e1ae3/jakarta.annotation-api-2.1.1.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework/spring-context/6.0.6/fbd2b7c23adb2ec2f7ca601b3e7d79ae10e342a/spring-context-6.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework/spring-aop/6.0.6/c95dc800fdce470519b7135272e64d4606fc8428/spring-aop-6.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework/spring-beans/6.0.6/48ea4ba141146b3acaad0c7df80d2a06afeb95fd/spring-beans-6.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework/spring-expression/6.0.6/2006ee0e1be8380f05c29deb52a97d3a1e6812d7/spring-expression-6.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework/spring-core/6.0.6/8a2845e0945923a9ebf3e9ef0649a28b6eeeac43/spring-core-6.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.yaml/snakeyaml/1.33/2cd0a87ff7df953f810c344bdf2fe3340b954c69/snakeyaml-1.33.jar:/home/woot/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.datatype/jackson-datatype-jsr310/2.14.2/796518148a385b2728d44886cc0f8852eb8eeb53/jackson-datatype-jsr310-2.14.2.jar:/home/woot/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.module/jackson-module-parameter-names/2.14.2/2b6c19b3d99dda02915515df879ab9e23fed3864/jackson-module-parameter-names-2.14.2.jar:/home/woot/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.core/jackson-annotations/2.14.2/a7aae9525864930723e3453ab799521fdfd9d873/jackson-annotations-2.14.2.jar:/home/woot/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.core/jackson-core/2.14.2/f804090e6399ce0cf78242db086017512dd71fcc/jackson-core-2.14.2.jar:/home/woot/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.datatype/jackson-datatype-jdk8/2.14.2/2f3c71211b6ea7a978eba33574d7135d536e07fb/jackson-datatype-jdk8-2.14.2.jar:/home/woot/.gradle/caches/modules-2/files-2.1/com.fasterxml.jackson.core/jackson-databind/2.14.2/1e71fddbc80bb86f71a6345ac1e8ab8a00e7134/jackson-databind-2.14.2.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.apache.tomcat.embed/tomcat-embed-websocket/10.1.5/14529cbd593571dc9029272ddc9166b5ef113fc2/tomcat-embed-websocket-10.1.5.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.apache.tomcat.embed/tomcat-embed-core/10.1.5/21417d3ef8189e2af05aae0a765ad9204d7211b5/tomcat-embed-core-10.1.5.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.apache.tomcat.embed/tomcat-embed-el/10.1.5/c125df13af42a0fc0cd342370449b1276181e2a1/tomcat-embed-el-10.1.5.jar:/home/woot/.gradle/caches/modules-2/files-2.1/io.micrometer/micrometer-observation/1.10.4/96aa92ad7a18c2451c429348c0167eb08a988d4e/micrometer-observation-1.10.4.jar:/home/woot/.gradle/caches/modules-2/files-2.1/ch.qos.logback/logback-classic/1.4.5/28e7dc0b208d6c3f15beefd73976e064b4ecfa9b/logback-classic-1.4.5.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.apache.logging.log4j/log4j-to-slf4j/2.19.0/30f4812e43172ecca5041da2cb6b965cc4777c19/log4j-to-slf4j-2.19.0.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.slf4j/jul-to-slf4j/2.0.6/c4d348977a83a0bfcf42fd6fd1fee6e7904f1a0c/jul-to-slf4j-2.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.springframework/spring-jcl/6.0.6/f13cd8a561e71fb65c63201113d766fcd7a2b06f/spring-jcl-6.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/io.micrometer/micrometer-commons/1.10.4/a5db33c573c8755e70388f204e83000dc359b63d/micrometer-commons-1.10.4.jar:/home/woot/.gradle/caches/modules-2/files-2.1/ch.qos.logback/logback-core/1.4.5/e9bb2ea70f84401314da4300343b0a246c8954da/logback-core-1.4.5.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.slf4j/slf4j-api/2.0.6/88c40d8b4f33326f19a7d3c0aaf2c7e8721d4953/slf4j-api-2.0.6.jar:/home/woot/.gradle/caches/modules-2/files-2.1/org.apache.logging.log4j/log4j-api/2.19.0/ea1b37f38c327596b216542bc636cfdc0b8036fa/log4j-api-2.19.0.jar com.example.demo.DemoApplication")]
    fn detect_javaagent_port(#[case] command_line: &str) {
        let debugger = DebuggerType::JavaAgent;

        assert_eq!(
            debugger.get_port(
                &command_line
                    .split_ascii_whitespace()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            ),
            Some(54898)
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
