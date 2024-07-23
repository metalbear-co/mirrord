use std::{ffi::OsStr, path::Path};

use exec::execvp;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_progress::{Progress, ProgressTracker};

use crate::{
    config::{ContainerArgs, ContainerCommand, ContainerRuntime},
    error::Result,
    execution::{MirrordExecution, INJECTION_ENV_VAR},
};

struct Empty;
struct WithCommand {
    command: ContainerCommand,
}

struct RuntimeCommandBuilder<T = Empty> {
    step: T,
    runtime: String,
    extra_args: Vec<String>,
}

impl RuntimeCommandBuilder {
    fn new(runtime: ContainerRuntime) -> Self {
        RuntimeCommandBuilder {
            step: Empty,
            runtime: runtime.to_string(),
            extra_args: Vec::new(),
        }
    }

    fn add_env<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let key = key.as_ref().to_str().unwrap_or_default();
        let value = value.as_ref().to_str().unwrap_or_default();

        self.extra_args.push("-e".to_owned());
        self.extra_args.push(format!("{key}={value}"))
    }

    fn add_envs<I, K, V>(&mut self, iter: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (key, value) in iter {
            self.add_env(key, value);
        }
    }

    fn add_volume<H, C>(&mut self, host_path: H, container_path: C)
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

    #[must_use]
    fn with_command(self, command: ContainerCommand) -> RuntimeCommandBuilder<WithCommand> {
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

pub(crate) async fn container_command(args: ContainerArgs, watch: drain::Watch) -> Result<()> {
    let progress = ProgressTracker::from_env("mirrord container");

    let mut config_env = args.params.to_env()?;

    for (name, value) in &config_env {
        std::env::set_var(name, value);
    }

    let (config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, watch);
    (&config).collect_analytics(analytics.get_mut());

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    let mut sub_progress = progress.subtask("preparing to launch process");

    #[cfg(target_os = "macos")]
    let mut execution_info =
        MirrordExecution::start(&config, None, &mut sub_progress, &mut analytics).await?;
    #[cfg(target_os = "linux")]
    let mut execution_info =
        MirrordExecution::start(&config, &mut sub_progress, &mut analytics).await?;

    tracing::info!(?execution_info, "starting");

    if let Some(connect_tcp) = execution_info.environment.get_mut("MIRRORD_CONNECT_TCP") {
        *connect_tcp = str::replace(connect_tcp, "127.0.0.1", "10.0.0.4");
    }

    #[cfg(target_os = "macos")]
    let library_path = {
        execution_info.environment.remove(INJECTION_ENV_VAR);

        let library_path = "/Users/dmitry/ws/metalbear/mirrord/target/aarch64-unknown-linux-gnu/debug/libmirrord_layer.so".to_owned();

        execution_info.environment.insert(
            crate::execution::LINUX_INJECTION_ENV_VAR.to_string(),
            "/tmp/libmirrord_layer.so".to_string(),
        );

        library_path
    };

    #[cfg(target_os = "linux")]
    let library_path = {
        let library_path = execution_info
            .environment
            .get(INJECTION_ENV_VAR)
            .cloned()
            .unwrap_or_default();

        execution_info.environment.insert(
            INJECTION_ENV_VAR.to_string(),
            "/tmp/libmirrord_layer.so".to_string(),
        );

        library_path
    };

    let mirrord_config_path = config_env
        .iter_mut()
        .find(|(key, _)| key == "MIRRORD_CONFIG_FILE")
        .map(|(_, config_path)| {
            let host_path = config_path.to_str().expect("should convert").to_owned();

            let config_name = Path::new(config_path)
                .file_name()
                .and_then(|os_str| os_str.to_str())
                .unwrap_or_default();

            let continer_path = format!("/tmp/{}", config_name);

            *config_path = (&continer_path).into();

            (host_path, continer_path)
        });

    let mut command = RuntimeCommandBuilder::new(args.runtime);

    command.add_envs(config_env.clone());
    command.add_envs(
        execution_info
            .environment
            .iter()
            .filter(|(key, _)| key != &"HOSTNAME"),
    );
    command.add_volume(library_path, "/tmp/libmirrord_layer.so");

    if let Some((host_path, continer_path)) = mirrord_config_path {
        command.add_volume(host_path, continer_path);
    }

    let (binary, binary_args) = command.with_command(args.command).into_execvp_args();
    let err = execvp(binary, binary_args);
    tracing::error!("Couldn't execute {:?}", err);

    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    // Kills the intproxy, freeing the agent.
    execution_info.stop().await;

    Ok(())
}
