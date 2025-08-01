use std::{borrow::Cow, ffi::OsStr, fmt};
#[cfg(not(target_os = "windows"))]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;

/// Convenience trait that allows for producing a nice display of an std/tokio command.
pub trait CommandExt {
    fn display(&self) -> CommandDisplay;
}

impl CommandExt for std::process::Command {
    fn display(&self) -> CommandDisplay {
        let envs = self.get_envs().map(|(name, value)| match value {
            #[cfg(not(windows))]
            Some(value) => {
                let mut buf =
                    Vec::with_capacity(name.as_bytes().len() + value.as_bytes().len() + 1);
                buf.extend_from_slice(name.as_bytes());
                buf.push(b'=');
                buf.extend_from_slice(value.as_bytes());
                String::from_utf8_lossy(&buf).into_owned()
            }
            #[cfg(target_os = "windows")]
            Some(value) => {
                let name_utf16: Vec<u16> = OsStr::new(name).encode_wide().collect();
                let value_utf16: Vec<u16> = OsStr::new(value).encode_wide().collect();

                let mut buf = Vec::with_capacity(name_utf16.len() + value_utf16.len() + 1);
                buf.extend(name_utf16);
                buf.push(b'=' as u16);
                buf.extend(value_utf16);
                String::from_utf16_lossy(&buf)
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
