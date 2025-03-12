use serde::Serialize;

use crate::config::{ContainerRuntime, ContainerRuntimeCommand};

#[derive(Debug, Clone)]
pub struct Empty;

#[derive(Debug, Clone)]
pub struct WithCommand {
    command: ContainerRuntimeCommand,
}

/// Builder for a container runtime command.
///
/// All environment variables and paths are accepted as [`str`],
/// because we might need to serialize them for the mirrord extensions.
#[derive(Debug, Clone)]
pub struct RuntimeCommandBuilder<T = Empty> {
    step: T,
    runtime: ContainerRuntime,
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
            extra_args: Vec::new(),
        }
    }

    pub fn add_env(&mut self, key: &str, value: &str) {
        self.push_arg("-e");
        self.push_arg(format!("{key}={value}"))
    }

    pub fn add_envs<'a, I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        for (key, value) in iter {
            self.add_env(key, value);
        }
    }

    pub fn add_volume(&mut self, host_path: &str, container_path: &str, readonly: bool) {
        match self.runtime {
            ContainerRuntime::Podman | ContainerRuntime::Docker | ContainerRuntime::Nerdctl => {
                self.push_arg("-v");
                self.push_arg(format!(
                    "{host_path}:{container_path}{}",
                    readonly.then_some(":ro").unwrap_or_default()
                ));
            }
        }
    }

    pub fn add_volumes_from<V>(&mut self, volumes_from: V)
    where
        V: Into<String>,
    {
        match self.runtime {
            ContainerRuntime::Podman | ContainerRuntime::Docker | ContainerRuntime::Nerdctl => {
                self.push_arg("--volumes-from");
                self.push_arg(volumes_from);
            }
        }
    }

    pub fn add_network<N>(&mut self, network: N)
    where
        N: Into<String>,
    {
        match self.runtime {
            ContainerRuntime::Podman | ContainerRuntime::Docker | ContainerRuntime::Nerdctl => {
                self.push_arg("--network");
                self.push_arg(network);
            }
        }
    }

    pub fn with_command(
        self,
        command: ContainerRuntimeCommand,
    ) -> RuntimeCommandBuilder<WithCommand> {
        let RuntimeCommandBuilder {
            runtime,
            extra_args,
            ..
        } = self;

        RuntimeCommandBuilder {
            step: WithCommand { command },
            runtime,
            extra_args,
        }
    }

    /// Convert to a serializable form to be used in mirrord extensions.
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
    /// Return completed command command with updated arguments.
    ///
    /// To be used when mirrord CLI executes the command.
    pub fn into_command_args(self) -> (String, impl Iterator<Item = String>) {
        let RuntimeCommandBuilder {
            runtime,
            extra_args,
            step,
        } = self;

        let (runtime_command, runtime_args) = step.command.into_parts();

        (
            runtime.to_string(),
            runtime_command
                .into_iter()
                .chain(extra_args)
                .chain(runtime_args),
        )
    }
}

/// Data required by the mirrord extensions for the mirrord container feature.
#[derive(Debug, Serialize)]
pub struct ExtensionRuntimeCommand {
    /// Container runtime to use (should be defaulted to docker)
    runtime: ContainerRuntime,

    /// Run command args that the extension should add to container command
    extra_args: Vec<String>,
}
