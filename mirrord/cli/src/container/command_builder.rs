use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    path::Path,
};

use tracing::Level;

use crate::config::{ContainerRuntime, ContainerRuntimeCommand};

#[derive(Debug, Clone)]
pub struct Empty;
#[derive(Debug, Clone)]
pub struct WithCommand {
    command: ContainerRuntimeCommand,
}

#[derive(Debug, Clone)]
pub struct RuntimeCommandBuilder<T = Empty> {
    step: T,
    runtime: ContainerRuntime,
    env_vars: HashMap<String, String>,
    volumes: HashMap<String, String>,
    /// `--volumes-from={container}`
    volumes_from: HashSet<String>,
    /// [`docker run --network`](https://docs.docker.com/engine/network/)
    ///
    /// [`docker compose` networks](https://docs.docker.com/compose/how-tos/networking/#specify-custom-networks).
    networks: HashSet<String>,
    extra_args: Vec<String>,
}

impl<T> RuntimeCommandBuilder<T> {
    fn push_arg<V>(&mut self, value: V)
    where
        V: Into<String>,
    {
        self.extra_args.push(value.into())
    }
}

impl RuntimeCommandBuilder {
    pub fn new(runtime: ContainerRuntime) -> Self {
        RuntimeCommandBuilder {
            step: Empty,
            runtime,
            extra_args: Default::default(),
            env_vars: Default::default(),
            volumes: Default::default(),
            volumes_from: Default::default(),
            networks: Default::default(),
        }
    }

    // TODO(alex) [high]: It's here, I can match on `Compose`, or even change this thing to
    // be `env_vars: Vec<EnvVar>`, and `extra_args: Vec<Things>` separately. Same for
    // the `add_volume`, since `-v` is probably not a compose thing either.
    pub(super) fn add_env<K, V>(&mut self, key: K, value: V) -> Option<String>
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let key = key.as_ref().to_str()?.into();
        let value = value.as_ref().to_str()?.into();

        self.env_vars.insert(key, value)
        // self.push_arg("-e");
        // self.push_arg(format!("{key}={value}"))
    }

    pub(super) fn add_envs<I, K, V>(&mut self, iter: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.env_vars
            .extend(iter.into_iter().filter_map(|(key, value)| {
                let key = key.as_ref().to_str()?;
                let value = value.as_ref().to_str()?;
                Some((key.into(), value.into()))
            }));

        // for (key, value) in iter {
        //     self.add_env(key, value);
        // }
    }

    #[tracing::instrument(level = Level::DEBUG, skip(self))]
    pub(super) fn add_volume<const READONLY: bool, H, C>(
        &mut self,
        host_path: H,
        container_path: C,
    ) -> Option<String>
    where
        H: AsRef<Path> + core::fmt::Debug,
        C: AsRef<Path> + core::fmt::Debug,
    {
        match self.runtime {
            ContainerRuntime::Podman | ContainerRuntime::Docker | ContainerRuntime::Nerdctl => {
                self.push_arg("--volume");

                let container_path = if READONLY {
                    format!("{}:ro", container_path.as_ref().display())
                    // self.push_arg(format!(
                    //     "{}:{}:ro",
                    //     host_path.as_ref().display(),
                    //     container_path.as_ref().display()
                    // ));
                } else {
                    container_path.as_ref().to_string_lossy().into_owned()
                    // self.push_arg(format!(
                    //     "{}:{}",
                    //     host_path.as_ref().display(),
                    //     container_path.as_ref().display()
                    // ));
                };

                self.volumes.insert(
                    host_path.as_ref().to_string_lossy().into_owned(),
                    container_path,
                )
            }
        }
    }

    pub(super) fn add_volumes_from<V>(&mut self, volumes_from: V)
    where
        V: Into<String>,
    {
        match self.runtime {
            ContainerRuntime::Podman | ContainerRuntime::Docker | ContainerRuntime::Nerdctl => {
                self.volumes_from.insert(volumes_from.into());
                // self.push_arg("--volumes-from");
                // self.push_arg(volumes_from);
            }
        }
    }

    pub(super) fn add_network<N>(&mut self, network: N)
    where
        N: Into<String>,
    {
        match self.runtime {
            ContainerRuntime::Podman | ContainerRuntime::Docker | ContainerRuntime::Nerdctl => {
                self.networks.insert(network.into());
                // self.push_arg("--network");
                // self.push_arg(network);
            }
        }
    }

    pub(super) fn with_command(
        self,
        command: ContainerRuntimeCommand,
    ) -> RuntimeCommandBuilder<WithCommand> {
        let RuntimeCommandBuilder {
            runtime,
            extra_args,
            env_vars,
            step: _,
            volumes,
            volumes_from,
            networks,
        } = self;

        RuntimeCommandBuilder {
            step: WithCommand { command },
            runtime,
            extra_args,
            env_vars,
            volumes,
            volumes_from,
            networks,
        }
    }

    /// Convert to serialzable result for mirrord extensions
    pub fn into_command_extension_params(self) -> ExtensionRuntimeCommand {
        let RuntimeCommandBuilder {
            runtime,
            extra_args,
            ..
        } = self;

        ExtensionRuntimeCommand {
            runtime,
            extra_args,
        }
    }
}

impl RuntimeCommandBuilder<WithCommand> {
    // TODO(alex) [high]: So this returns a command with `-e ENV_VAR -e OTHER_VAR`, and
    // `compose` only supports this flag with `compose run -e`, which is not `compose up`.
    // These env vars should be part of the compose file, which means it's time to load
    // the user's compose file, add these bunch of env vars to it (as a temp/memory file),
    // and that's what we pass to the container we're creating.
    // I need some sort of `Deserialize` `ComposeFile` struct now to pass around.
    //
    // Plus I probably need to remove other commands that are being inserted.
    //
    //
    /**
    "return=(\"docker\", Chain { a: Some(Chain { a: Some(IntoIter([\"compose\"])), b: Some(IntoIter([\"-e\", \"MIRRORD_PROGRESS_MODE=off\", \"-e\", \"MIRRORD_EXECUTION_KIND=1\", \"-e\", \"MIRRORD_CONFIG_FILE=/tmp/mirrord-config.json\", \"-v\", \"/tmp/.tmpQZZVYW.json:/tmp/mirrord-config.json:ro\", \"-e\", \"MIRRORD_INTPROXY_CLIENT_TLS_CERTIFICATE=/tmp/mirrord_intproxy_client_tls_certificate.pem\", \"-v\", \"/tmp/.tmpttuPBp:/tmp/mirrord_intproxy_client_tls_certificate.pem:ro\", \"-e\", \"MIRRORD_INTPROXY_CLIENT_TLS_KEY=/tmp/mirrord_intproxy_client_tls_key.pem\", \"-v\", \"/tmp/.tmpubj8bl:/tmp/mirrord_intproxy_client_tls_key.pem:ro\", \"-e\", \"LD_PRELOAD=/opt/mirrord/lib/libmirrord_layer.so\", \"-e\", \"MIRRORD_CONNECT_TCP=127.0.0.1:53679\"])) }), b: Some(IntoIter([\"foo\"])) })"
    */
    //
    // And to pass files, search for the places where the container command interacts with `/tmp`.
    /// Return completed command command with updated arguments
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub(super) fn into_command_args(self) -> (String, impl Iterator<Item = String>) {
        let RuntimeCommandBuilder {
            runtime,
            extra_args,
            step,
            env_vars,
            volumes,
            networks,
            volumes_from,
        } = self;

        let (runtime_command, runtime_args) = step.command.into_parts();

        // TODO(alex) [high]: Somehow, I want to return the args for compose here, but not
        // make it immediatelly used, as I want to put it in a file/tmp. These args cannot be
        // `chain`ed for compose. What if I modify it before we get here? It probably makes more
        // sense to look where `extra_args` is coming from, and maybe there we can
        // differentiate between compose command and others, then create the file, so these
        // extras don't get here.
        //
        // [update]: I have added separate fields for each option, except that some might
        // still be leaking through `extra_args`. Fix the `mirrord container docker run` command
        // to work with this refactor before moving on to `compose`.
        (
            runtime.to_string(),
            runtime_command
                .into_iter()
                .chain(extra_args)
                .chain(runtime_args),
        )
    }
}

/// Information for mirrord extensions to use mirrord container feature but injecting the connection
/// info into the container manualy by the extension
#[derive(Debug, serde::Serialize)]
pub struct ExtensionRuntimeCommand {
    /// Container runtime to use (should be defaulted to docker)
    runtime: ContainerRuntime,

    /// Run command args that the extension should add to container command
    extra_args: Vec<String>,
}
