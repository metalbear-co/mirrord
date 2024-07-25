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
    runtime: String,
    extra_args: Vec<String>,
}

impl<T> RuntimeCommandBuilder<T> {
    pub fn push_arg<V>(&mut self, value: V)
    where
        V: Into<String>,
    {
        self.extra_args.push(value.into())
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

    pub fn add_volume<H, C>(&mut self, host_path: H, container_path: C)
    where
        H: AsRef<Path>,
        C: AsRef<Path>,
    {
        self.push_arg("-v");
        self.push_arg(format!(
            "{}:{}",
            host_path.as_ref().display(),
            container_path.as_ref().display()
        ));
    }
}

impl RuntimeCommandBuilder {
    pub fn new(runtime: ContainerRuntime) -> Self {
        RuntimeCommandBuilder {
            step: Empty,
            runtime: runtime.to_string(),
            extra_args: Vec::new(),
        }
    }

    #[must_use]
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
    pub fn into_execvp_args(self) -> (String, Vec<String>) {
        let RuntimeCommandBuilder {
            runtime,
            extra_args,
            step,
        } = self;

        let (runtime_command, runtime_args) = match step.command {
            ContainerCommand::Run { runtime_args } => ("run".to_owned(), runtime_args),
        };

        (
            runtime.clone(),
            std::iter::once(runtime)
                .chain(std::iter::once(runtime_command))
                .chain(extra_args)
                .chain(runtime_args)
                .collect(),
        )
    }
}
