use std::{borrow::Cow, ffi::OsStr, fmt};

/// Convenience trait that allows for producing a nice display of an std/tokio command.
pub trait CommandExt {
    fn display(&self) -> CommandDisplay;
}

impl CommandExt for std::process::Command {
    fn display(&self) -> CommandDisplay {
        let envs = self.get_envs().map(|(name, value)| match value {
            Some(value) => {
                // Create the environment string in the standard format
                format!("{}={}", name.to_string_lossy(), value.to_string_lossy())
            }
            None => name.to_string_lossy().into_owned(),
        });
        let program = std::iter::once(self.get_program().to_string_lossy().into_owned());
        let args = self
            .get_args()
            .map(OsStr::to_string_lossy)
            .map(Cow::into_owned);

        CommandDisplay(
            envs.chain(program)
                .chain(args)
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

impl CommandExt for tokio::process::Command {
    fn display(&self) -> CommandDisplay {
        self.as_std().display()
    }
}

/// A human readable display of a command.
///
/// For example: `docker run container`.
pub struct CommandDisplay(String);

impl fmt::Display for CommandDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Debug for CommandDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
