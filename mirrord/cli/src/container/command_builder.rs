use std::{ffi::OsStr, path::Path};

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
    extra_args: Vec<String>,
}

impl<T> RuntimeCommandBuilder<T> {
    pub(super) fn runtime(&self) -> &ContainerRuntime {
        &self.runtime
    }

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

    pub(super) fn add_env<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let key = key.as_ref().to_str().unwrap_or_default();
        let value = value.as_ref().to_str().unwrap_or_default();

        self.push_arg("-e");
        self.push_arg(format!("{key}={value}"))
    }

    pub(super) fn add_envs<I, K, V>(&mut self, iter: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (key, value) in iter {
            self.add_env(key, value);
        }
    }

    pub(super) fn add_volume<const READONLY: bool, H, C>(&mut self, host_path: H, container_path: C)
    where
        H: AsRef<Path>,
        C: AsRef<Path>,
    {
        match self.runtime {
            ContainerRuntime::Podman | ContainerRuntime::Docker | ContainerRuntime::Nerdctl => {
                self.push_arg("-v");

                if READONLY {
                    self.push_arg(format!(
                        "{}:{}:ro",
                        host_path.as_ref().display(),
                        container_path.as_ref().display()
                    ));
                } else {
                    self.push_arg(format!(
                        "{}:{}",
                        host_path.as_ref().display(),
                        container_path.as_ref().display()
                    ));
                }
            }
        }
    }

    pub(super) fn add_volumes_from<V>(&mut self, volumes_from: V)
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

    pub(super) fn add_network<N>(&mut self, network: N)
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

    pub(super) fn with_command(
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
    /// Return completed command command with updated arguments
    pub(super) fn into_command_args(self) -> (String, impl Iterator<Item = String>) {
        let RuntimeCommandBuilder {
            runtime,
            extra_args,
            step,
        } = self;

        let (runtime_command, runtime_args) = match step.command {
            ContainerRuntimeCommand::Run { runtime_args } => ("run".to_owned(), runtime_args),
        };

        (
            runtime.to_string(),
            std::iter::once(runtime_command)
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
