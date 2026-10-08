//! The environment block a process creation was given.
//!
//! The block itself is read by `WindowsEnv::from_block` in
//! `mirrord_layer_lib::process::windows::environment`, which also compares names the way Windows
//! compares them. This module only decides whether there is a block, and in which encoding.

use std::ffi::c_void;

use mirrord_layer_lib::process::windows::environment::WindowsEnv;
use winapi::um::winbase::CREATE_UNICODE_ENVIRONMENT;

/// Parse the environment block a process creation was given.
///
/// The environment block can be either ANSI or Unicode depending on the creation flags.
/// This function checks the CREATE_UNICODE_ENVIRONMENT flag to determine the format, and reads
/// the block with [`WindowsEnv::from_block`].
///
/// # Returns
///
/// `None` for a null block, which inherits this process's environment. Otherwise the caller's
/// environment, empty for a block that holds only its terminator, which asks for no variables at
/// all.
///
/// # Safety
/// This function is unsafe because it dereferences raw pointers from the Windows API.
/// The caller must ensure that the environment pointer is valid and properly formatted.
pub unsafe fn parse_caller_environment(
    environment: *mut c_void,
    creation_flags: u32,
) -> Option<WindowsEnv> {
    if environment.is_null() {
        return None;
    }

    Some(if creation_flags & CREATE_UNICODE_ENVIRONMENT != 0 {
        unsafe { WindowsEnv::from_block::<u16>(environment.cast()) }
    } else {
        unsafe { WindowsEnv::from_block::<u8>(environment.cast()) }
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;

    use super::*;

    /// Encodes `entries` as a Unicode environment block, in the given order.
    fn unicode_block(entries: &[&str]) -> Vec<u16> {
        let mut block = entries
            .iter()
            .flat_map(|entry| entry.encode_utf16().chain([0]))
            .collect::<Vec<_>>();
        block.push(0);
        if entries.is_empty() {
            block.push(0);
        }
        block
    }

    /// Parses `entries` the way the hook parses a caller's Unicode block.
    fn parse(entries: &[&str]) -> WindowsEnv {
        let mut block = unicode_block(entries);
        unsafe {
            parse_caller_environment(
                block.as_mut_ptr() as *mut c_void,
                CREATE_UNICODE_ENVIRONMENT,
            )
        }
        .expect("an explicit block")
    }

    /// A null block inherits; a block that holds only its terminator asks for no variables.
    #[test]
    fn an_empty_block_is_not_an_inherited_one() {
        assert_eq!(
            unsafe { parse_caller_environment(std::ptr::null_mut(), CREATE_UNICODE_ENVIRONMENT) },
            None
        );

        assert_eq!(parse(&[]), WindowsEnv::new());

        let mut ansi_empty = [0u8, 0u8];
        assert_eq!(
            unsafe { parse_caller_environment(ansi_empty.as_mut_ptr() as *mut c_void, 0) },
            Some(WindowsEnv::new())
        );
    }

    /// The creation flags pick the block's encoding.
    #[test]
    fn the_creation_flags_pick_the_encoding() {
        let expected = Some(WindowsEnv::from_ordered_entries([(
            "A".to_owned(),
            "1".to_owned(),
        )]));

        let mut ansi = *b"A=1\0\0";
        assert_eq!(
            unsafe { parse_caller_environment(ansi.as_mut_ptr() as *mut c_void, 0) },
            expected
        );
        assert_eq!(Some(parse(&["A=1"])), expected);
    }

    /// Windows limits one variable to 32767 characters, but not the block: a pod's environment
    /// easily passes 32768 units, and all of it reaches the child.
    #[test]
    fn a_block_larger_than_32768_units_is_kept_whole() {
        let entries = (0..800)
            .map(|i| {
                format!(
                    "SERVICE_{i:04}_PORT_8080_TCP_ADDR=10.96.{}.{}",
                    i / 250,
                    i % 250
                )
            })
            .collect::<Vec<_>>();
        let environment = parse(&entries.iter().map(String::as_str).collect::<Vec<_>>());

        assert_eq!(environment.len(), entries.len());
        assert_eq!(
            environment.get("SERVICE_0799_PORT_8080_TCP_ADDR"),
            Some("10.96.3.49")
        );
    }
}
