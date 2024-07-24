use std::{ffi::OsStr, path::Path};

use crate::config::{ContainerCommand, ContainerRuntime};

pub struct Empty;
pub struct WithCommand {
    command: ContainerCommand,
}

pub struct RuntimeCommandBuilder<T = Empty> {
    step: T,
    runtime: String,
    extra_args: Vec<String>,
}

impl<T> RuntimeCommandBuilder<T> {
    pub fn add_env<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let key = key.as_ref().to_str().unwrap_or_default();
        let value = value.as_ref().to_str().unwrap_or_default();

        self.extra_args.push("-e".to_owned());
        self.extra_args.push(format!("{key}={value}"))
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
        self.extra_args.push("-v".to_owned());
        self.extra_args.push(format!(
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
    pub fn add_entrypoint<S>(&mut self, entrypoint: S)
    where
        S: Into<String>,
    {
        if let Some(in_command) = self.entrypoint_mut() {
            *in_command = entrypoint.into();
        } else {
            self.extra_args.push("--entrypoint".to_owned());
            self.extra_args.push(entrypoint.into());
        }
    }

    pub fn entrypoint(&self) -> Option<&str> {
        let entrypoint_index = self.entrypoint_index();

        let ContainerCommand::Run { runtime_args } = &self.step.command;
        entrypoint_index
            .and_then(|index| runtime_args.get(index))
            .map(|entrypoint| entrypoint.as_str())
    }

    fn entrypoint_index(&self) -> Option<usize> {
        let ContainerCommand::Run { runtime_args } = &self.step.command;

        let mut runtime_args_iter = runtime_args.iter().enumerate();

        loop {
            match runtime_args_iter.next() {
                Some((_, arg)) if arg == "--entrypoint" => {
                    break runtime_args_iter.next().map(|(index, _)| index)
                }
                None => break None,
                _ => {}
            }
        }
    }

    fn entrypoint_mut(&mut self) -> Option<&mut String> {
        let entrypoint_index = self.entrypoint_index();

        let ContainerCommand::Run { runtime_args } = &mut self.step.command;

        entrypoint_index.and_then(|index| runtime_args.get_mut(index))
    }

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
