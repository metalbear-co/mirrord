use std::io::Write;

use serde::Serialize;
use thiserror::Error;

macro_rules! setup_writer {
    ($($tt:tt)*) => {
        $crate::setup::writer::setup_writer_parse!{ $($tt)* }
    }
}

macro_rules! setup_writer_parse {
    ($(#[$attr:ident($($att:tt)*)])+ $($tt:tt)*) => {
        $crate::setup::writer::setup_writer_parse_with_args!{ [$($attr($($att)*),)+] $($tt)* }
    };
    ($($tt:tt)*) => {
        $crate::setup::writer::setup_writer_parse_with_args!{ [] $($tt)* }
    }
}

macro_rules! setup_writer_parse_with_args {
    ([$($attr:ident($($att:tt)*),)*] $vis:vis struct $ident:ident $($tt:tt)*) => {
        $(#[$attr($($att)*)])*
        $vis struct $ident $($tt)*

        $crate::setup::writer::setup_writer_struct_parse!($ident $($tt)*);
    };
}

macro_rules! setup_writer_struct_parse {
    ($ident:ident { $($vis:vis $arg:ident: $ty:ty,)* }) => {
        $crate::setup::writer::setup_writer_struct_impl!([$($arg,)*] $ident);
    };
    ($ident:ident ($arg0:ty, $arg1:ty, $arg2:ty);) => {
        $crate::setup::writer::setup_writer_struct_impl!([0, 1, 2,] $ident);
    };
    ($ident:ident ($arg0:ty, $arg1:ty);) => {
        $crate::setup::writer::setup_writer_struct_impl!([0, 1,] $ident);
    };
    ($ident:ident ($arg0:ty);) => {
        $crate::setup::writer::setup_writer_struct_impl!([0,] $ident);
    };
}

macro_rules! setup_writer_struct_impl {
    ([$($field:tt,)*] $ident:ident) => {
        impl $crate::setup::writer::SetupWriter for $ident {
            fn to_writer<W: std::io::Write>(&self, mut writer: W) -> Result<(), $crate::setup::writer::SetupWriteError> {
                $(
                    $crate::setup::writer::SetupWriter::to_writer(&self.$field, &mut writer)?;
                )*

                Ok(())
            }
        }
    };
}

pub(crate) use setup_writer;
pub(crate) use setup_writer_parse;
pub(crate) use setup_writer_parse_with_args;
pub(crate) use setup_writer_struct_impl;
pub(crate) use setup_writer_struct_parse;

/// General Operator Error
#[derive(Debug, Error)]
pub enum SetupWriteError {
    #[error(transparent)]
    YamlSerialization(#[from] serde_yaml::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub trait SetupWriter {
    fn to_writer<W: Write>(&self, writer: W) -> Result<(), SetupWriteError>;
}

impl<T> SetupWriter for T
where
    T: Serialize,
{
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<(), SetupWriteError> {
        writer.write_all(b"---\n")?;

        serde_yaml::to_writer(writer, &self).map_err(SetupWriteError::from)
    }
}
