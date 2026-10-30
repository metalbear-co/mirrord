//! The debug hold's two hops. Each has its own variable and one reader, and nothing goes further.
//!
//! 1. [`MIRRORD_DEBUG_HOLD`]`=1`: `mirrord up --debug <svc>` asks that service's `mirrord exec` to
//!    hold the program it starts. Only `exec` reads it, and the program never gets it.
//! 2. That `exec` hands the hold on, by platform:
//!    - Windows: `exec` creates the program and stays its parent, so it holds the program itself,
//!      from outside. The program gets nothing.
//!    - Linux and macOS: `exec` replaces itself with the program through `execve`, so no `exec`
//!      code is left to hold it, and the program's own layer holds instead. The program gets
//!      [`MIRRORD_LAYER_DEBUG_HOLD`]`=<exec's pid>`, which `execve` keeps, so only that process
//!      matches.
//!
//! Two names rather than two values of one: a `mirrord exec` the program starts reads only
//! [`MIRRORD_DEBUG_HOLD`], so nothing the program's layer is told can make that `exec` hold too,
//! with no IDE waiting for it.

use std::ffi::OsStr;

pub use crate::{MIRRORD_DEBUG_HOLD, MIRRORD_LAYER_DEBUG_HOLD};

/// The value of [`MIRRORD_DEBUG_HOLD`] that asks for a hold. Any other is no request.
pub const REQUESTED: &str = "1";

/// Whether this process's environment carries `mirrord up`'s request.
pub fn requested_in_env() -> bool {
    std::env::var_os(MIRRORD_DEBUG_HOLD).is_some_and(|value| value == REQUESTED)
}

/// The pid a value of [`MIRRORD_LAYER_DEBUG_HOLD`] names: decimal digits only, anything else is
/// `None`.
pub fn layer_pid(value: &OsStr) -> Option<u32> {
    let value = value.to_str()?;
    // Digits only: `u32::from_str` also takes a leading `+`.
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::a_pid("4242", Some(4242))]
    // The odd one out: an `exec` running as pid 1, as in a container, is still a pid here, since
    // `mirrord up`'s request travels under another name.
    #[case::pid_one("1", Some(1))]
    #[case::signed("+42", None)]
    #[case::empty("", None)]
    #[case::anything_else("pid:42", None)]
    fn a_layer_pid_is_decimal_digits_only(#[case] value: &str, #[case] expected: Option<u32>) {
        assert_eq!(layer_pid(OsStr::new(value)), expected, "{value:?}");
    }
}
