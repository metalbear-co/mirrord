use std::{ffi::OsStr, path::Path};

use crate::config::{ContainerCommand, ContainerRuntime};

#[derive(Debug, Clone)]
pub struct Empty;
#[derive(Debug, Clone)]
pub struct WithCommand {
    command: ContainerCommand,
}

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

    pub fn add_env<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let key = key.as_ref().to_str().unwrap_or_default();
        let value = value.as_ref().to_str().unwrap_or_default();

        self.push_arg("-e");
        self.push_arg(format!("{key}={value}"))
    }

    pub fn add_envs<I, K, V>(&mut self, iter: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (key, value) in iter {
            self.add_env(key, value);
        }
    }

    pub fn add_publish<P>(&mut self, value: P)
    where
        P: AsRef<OsStr>,
    {
        self.push_arg("-p");
        self.push_arg(value.as_ref().to_str().unwrap_or_default())
    }

    pub fn add_port_bindings<I, P>(&mut self, iter: I)
    where
        I: IntoIterator<Item = P>,
        P: AsRef<OsStr>,
    {
        for publish in iter {
            self.add_publish(publish)
        }
    }

    pub fn add_volume<H, C>(&mut self, host_path: H, container_path: C)
    where
        H: AsRef<Path>,
        C: AsRef<Path>,
    {
        match self.runtime {
            ContainerRuntime::Podman | ContainerRuntime::Docker | ContainerRuntime::Nerdctl => {
                self.push_arg("-v");
                self.push_arg(format!(
                    "{}:{}",
                    host_path.as_ref().display(),
                    container_path.as_ref().display()
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

    pub fn with_command(self, command: ContainerCommand) -> RuntimeCommandBuilder<WithCommand> {
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
}

impl RuntimeCommandBuilder<WithCommand> {
    /// Return completed command command with updated arguments
    pub fn into_command_args(self) -> (String, impl Iterator<Item = String>) {
        let RuntimeCommandBuilder {
            runtime,
            extra_args,
            step,
        } = self;

        let (runtime_command, runtime_args) = match step.command {
            ContainerCommand::Run { runtime_args, .. } => ("run".to_owned(), runtime_args),
        };

        (
            runtime.to_string(),
            std::iter::once(runtime_command)
                .chain(extra_args)
                .chain(runtime_args),
        )
    }

    /// Same as [`RuntimeCommandBuilder::<WithCommand>::into_command_args`] execvp expects first arg
    /// to be binary so we pass the same value as runtime as first element
    pub fn into_execvp_args(self) -> (String, impl Iterator<Item = String>) {
        let (runtime, args) = self.into_command_args();

        (runtime.clone(), std::iter::once(runtime).chain(args))
    }
}
