use std::{borrow::Cow, ffi::OsStr, fmt, os::unix::ffi::OsStrExt};

use tokio::process::Command;

/// Convenience trait that allows for producing a nice display of [`Command`].
pub trait CommandExt {
    fn display(&self) -> CommandDisplay;
}

impl CommandExt for Command {
    fn display(&self) -> CommandDisplay {
        let envs = self.as_std().get_envs().map(|(name, value)| match value {
            Some(value) => {
                let mut buf =
                    Vec::with_capacity(name.as_bytes().len() + value.as_bytes().len() + 1);
                buf.extend_from_slice(name.as_bytes());
                buf.push(b'=');
                buf.extend_from_slice(value.as_bytes());
                String::from_utf8_lossy(&buf).into_owned()
            }
            None => name.to_string_lossy().into_owned(),
        });
        let program = std::iter::once(self.as_std().get_program().to_string_lossy().into_owned());
        let args = self
            .as_std()
            .get_args()
            .map(OsStr::to_string_lossy)
            .map(Cow::into_owned);

        CommandDisplay(envs.chain(program).chain(args).collect())
    }
}

/// A human readable display of a [`Command`].
///
/// For example: `docker run container`.
pub struct CommandDisplay(Vec<String>);

impl fmt::Display for CommandDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for chunk in &self.0 {
            if first {
                f.write_str(chunk)?;
                first = false;
            } else {
                write!(f, ", {chunk}")?;
            }
        }

        Ok(())
    }
}

impl fmt::Debug for CommandDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
