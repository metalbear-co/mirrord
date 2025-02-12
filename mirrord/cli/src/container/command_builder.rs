use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    io::{Read, Write},
    path::Path,
};

use tempfile::NamedTempFile;
use tokio::fs::OpenOptions;
use tracing::Level;

use crate::{
    config::{ContainerRuntime, ContainerRuntimeCommand},
    error::ContainerError,
};

#[derive(Debug, Clone)]
pub struct Empty;
#[derive(Debug, Clone)]
pub struct WithCommand {
    command: ContainerRuntimeCommand,
}

/// Turns a `HashMap` into `["--{arg}", "{key}{separator}{value}"]`,
/// e.g. `["--env", MEOW_LEVEL=HIGH]`.
#[tracing::instrument(level = Level::DEBUG, skip_all)]
fn map_into_args(map: HashMap<String, String>, arg: &str, separator: &str) -> Vec<String> {
    map.into_iter()
        .map(|(k, v)| format!("{k}{separator}{v}"))
        .fold(Vec::new(), |mut acc, e| {
            acc.push(arg.to_string());
            acc.push(e);
            acc
        })
}

/// Turns a `HashSet` into `["--{arg}", "{value}"]`, e.g. `["--network", "ethmeow"]`.
#[tracing::instrument(level = Level::DEBUG, skip_all)]
fn set_into_args(set: HashSet<String>, arg: &str) -> Vec<String> {
    set.into_iter().fold(Vec::new(), |mut acc, e| {
        acc.push(arg.to_string());
        acc.push(e);
        acc
    })
}

#[tracing::instrument(level = Level::DEBUG, skip(env_vars), ret, err)]
fn create_env_file(env_vars: HashMap<String, String>) -> Result<NamedTempFile, ContainerError> {
    let mut env_file = tempfile::Builder::new()
        .suffix(".env")
        .tempfile()
        .map_err(ContainerError::ConfigWrite)?;

    let env_vars = env_vars
        .into_iter()
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect::<String>();

    env_file
        .write_all(env_vars.as_bytes())
        .map_err(ContainerError::ConfigWrite)?;

    Ok(env_file)
}

#[tracing::instrument(level = Level::DEBUG, skip(env_vars), ret, err)]
fn create_compose_file(
    env_vars: HashMap<String, String>,
    volumes: HashMap<String, String>,
    volumes_from: HashSet<String>,
) -> Result<NamedTempFile, ContainerError> {
    use std::{fs::File, io::BufReader};

    let user_compose_file = File::open("./compose.yaml").unwrap();
    let reader = BufReader::new(user_compose_file);
    let mut read: serde_yaml::Value = serde_yaml::from_reader(reader).unwrap();

    tracing::debug!(?read, "What's in the file?");
    println!("What's in the file? {read:?}");

    let volumes_from_keys = volumes_from
        .iter()
        .filter_map(|v| v.split_terminator(":").next())
        .collect::<HashSet<_>>();

    {
        let services = read.get_mut("services").unwrap().as_mapping_mut().unwrap();

        for (_service_key, service_v) in services.iter_mut() {
            if let Some(file_envs) = service_v
                .get_mut("environment")
                .and_then(|env| env.as_mapping_mut())
            {
                for (k, v) in env_vars.iter() {
                    file_envs.insert(
                        serde_yaml::from_str(k).unwrap(),
                        serde_yaml::Value::String(format!(r#"{v}"#)),
                    );
                }
            }

            // TODO(alex) [#2]: Looks like it doesn't create the `/opt/mirrord` dir, meanwhile
            // `docker run` creates it, what's the difference between `compose run` here?.
            // In both cases we start with `create` first, so the cli image is created, and then
            // we call `run/compose`, so the image should have the the dir...
            if let Some(file_volumes) = service_v
                .get_mut("volumes")
                .and_then(|volume| volume.as_sequence_mut())
            {
                for (k, v) in volumes.iter() {
                    file_volumes.push(serde_yaml::Value::String(format!(r#"{k}:{v}"#)));
                }

                for v in volumes_from.iter() {
                    file_volumes.push(serde_yaml::Value::String(format!(r#"{v}:/external"#)));
                }
            }
        }
    }

    {
        // TODO(alex) [#3]: `--volumes-from` is breaking it, how do I make it work in compose?
        //
        // [update]: It has to be a mapping in the services, such as `volumes: random:/tmp`,
        // where it cannot be just `/`.
        // It must also be present in a separate `volumes` listing, after all the `services`.
        //
        // This was not enough to create the `/opt/mirrord` dir though.
        let volumes_file = read.get_mut("volumes").unwrap().as_mapping_mut().unwrap();
        // for k in volumes.keys() {
        //     volumes_file.insert(
        //         serde_yaml::Value::String(format!(r#"{k}"#)),
        //         serde_yaml::Value::Null,
        //     );
        // }

        for v in volumes_from_keys.iter() {
            let mapping =
                serde_yaml::from_str::<serde_yaml::Value>(format!("{v}:\nexternal: true").as_str())
                    .unwrap();
            let mapping = mapping.as_mapping().unwrap().clone();
            volumes_file.insert(
                mapping.keys().next().unwrap().clone(),
                mapping.values().next().unwrap().clone(),
            );
        }
    }

    let mut compose_file = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .map_err(ContainerError::ConfigWrite)?;

    compose_file
        .write_all(serde_yaml::to_string(&read).unwrap().as_bytes())
        .map_err(ContainerError::ConfigWrite)?;

    Ok(compose_file)
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
                // self.push_arg("--volume");

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

    /// Adds `/opt/mirrord` containing mirrord stuff used to `LD_PRELOAD` in the container.
    /// This volume comes from the `VOLUME /opt/mirrord` in the mirrord-cli `Dockerfile`.
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
    pub(super) fn into_command_args(self) -> (String, Vec<String>) {
        // pub(super) fn into_command_args(self) -> (String, Vec<String>) {
        let RuntimeCommandBuilder {
            runtime,
            extra_args,
            step,
            env_vars,
            volumes,
            networks,
            volumes_from,
        } = self;

        match &step.command {
            ContainerRuntimeCommand::Create { .. } | ContainerRuntimeCommand::Run { .. } => {
                let (runtime_command, runtime_args) = step.command.into_parts();

                // let env_vars = create_env_file(env_vars).unwrap();
                // extra_args.push("--env-file".to_string());
                // extra_args.push(env_vars.path().to_string_lossy().to_string());

                // volumes.insert(
                //     env_vars.path().to_string_lossy().into_owned(),
                //     "/tmp/env-file.env:ro".to_owned(),
                // );
                // env_vars.keep().unwrap();

                // let env_vars = env_vars.into_iter().map(|(k, v)| format!("{k}={v}")).fold(
                //     Vec::new(),
                //     |mut acc, e| {
                //         acc.push("--env".to_string());
                //         acc.push(e);
                //         acc
                //     },
                // );

                // let volumes = volumes.into_iter().map(|(k, v)| format!("{k}:{v}")).fold(
                //     Vec::new(),
                //     |mut acc, e| {
                //         acc.push("--volume".to_string());
                //         acc.push(e);
                //         acc
                //     },
                // );

                // let networks = networks.into_iter().fold(Vec::new(), |mut acc, e| {
                //     acc.push("--network".to_string());
                //     acc.push(e);
                //     acc
                // });

                // let volumes_from = volumes_from.into_iter().fold(Vec::new(), |mut acc, e| {
                //     acc.push("--volumes-from".to_string());
                //     acc.push(e);
                //     acc
                // });

                // TODO(alex) [mid]: Should probably detect multiple `network` and maybe
                // `volumes-from`, since these are not a thing for `docker run`, only for `compose`.
                // If we wait until here to give an error, then it might not be nicely reported to
                // the user.
                //
                // [update]: Instead of this crappy chain, since I'm doing the same thing for compose,
                // let's just create a docker file for this stuff, so I can fill it with the args,
                // create a temp file with all the env vars, mounts, networks, whatever, then just
                // mount it somehow and load it (the `docker create` is being run in the user's
                // machine, so just putting it in `/tmp` might be good enough).
                //
                // [update]: These args are passed to rust args thingy, which means it can't
                // be just one string arg, it has to be `"-volume", "/foo"`, and not `"-volume
                // foo"`.
                //
                // Make a function to _argify_ this stuff, for now I just need to fix the
                // `--env-file /tmp/foo.env` which is not being found, for some reason. Do I need
                // to mount it somehow? Solving this is also solving the `compose.yaml`.
                let command = runtime_command
                    .into_iter()
                    .chain(map_into_args(volumes, "--volume", ":"))
                    .chain(map_into_args(env_vars, "--env", "="))
                    .chain(set_into_args(networks, "--network"))
                    .chain(set_into_args(volumes_from, "--volumes-from"))
                    .chain(extra_args)
                    .chain(runtime_args)
                    .collect();

                (runtime.to_string(), command)
            }
            ContainerRuntimeCommand::Compose { runtime_args } => {
                let (runtime_command, runtime_args) = step.command.into_parts();

                let env_vars_file = create_env_file(env_vars.clone()).unwrap();
                let (file, path) = env_vars_file.keep().unwrap();

                let compose_file =
                    create_compose_file(env_vars.clone(), volumes.clone(), volumes_from.clone())
                        .unwrap();
                let (file, path) = compose_file.keep().unwrap();

                let command = runtime_command
                    .into_iter()
                    // .chain(vec![
                    //     "--env-file".to_string(),
                    //     path.to_string_lossy().into(),
                    // ])
                    .chain(vec!["--file".to_string(), path.to_string_lossy().into()])
                    .chain(extra_args)
                    .chain(runtime_args)
                    .collect();

                (runtime.to_string(), command)
            }
        }

        // let (runtime_command, runtime_args) = step.command.into_parts();

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
        // (
        //     runtime.to_string(),
        //     runtime_command
        //         .into_iter()
        //         .chain(extra_args)
        //         .chain(runtime_args),
        // )
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
