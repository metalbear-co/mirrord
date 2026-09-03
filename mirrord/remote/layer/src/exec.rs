//! Carries the layer's `LD_PRELOAD` into an `exec` that supplies its own environment.
//!
//! The bootstrap puts remote-layer on `LD_PRELOAD` in this process, so children
//! that inherit the environment load it without any interposition — see
//! `configure_preload` in the bootstrap crate.
//!
//! Inheritance only reaches a child that is given the parent's environment.
//! `execve` takes an explicit `envp`, and a caller that builds one — a shell, a
//! supervisor, a runtime spawning a subprocess with a chosen environment —
//! produces a new image with no layer and therefore no socket hooks, which then
//! serves its own connections rather than offering them to the agent. Nothing
//! reports this; the process simply stops taking part.
//!
//! Only `execve` is hooked. The `execv`/`execvp`/`execl` family passes `environ`,
//! which already carries the preload, so they need no help.
//!
//! The rebuilt environment carries [`REMOTE_LAYER_PATH_ENV`] as well as the
//! preload, so the new image can do this again for its own children.

use std::{
    ffi::{CStr, CString, OsStr, c_char},
    os::unix::ffi::OsStrExt,
    ptr,
};

use libc::c_int;
use mirrord_layer_core::{hooks::HookManager, replace};
use mirrord_layer_macro::hook_fn;
use mirrord_remote_layer_protocol::REMOTE_LAYER_PATH_ENV;

const LD_PRELOAD_ENV: &str = "LD_PRELOAD";

/// Holds replacement environment strings, so they can be handed back to C.
#[derive(Default, Debug, Clone)]
struct Envp(Vec<CString>);

impl Envp {
    /// Reads a null-terminated C list into owned strings.
    ///
    /// # Safety
    ///
    /// `raw` must be null, or a null-terminated array of valid C strings.
    unsafe fn from_raw(raw: *const *const c_char) -> Self {
        let mut collected = Vec::new();

        if raw.is_null() {
            return Self(collected);
        }

        let mut index = 0;
        loop {
            let entry = unsafe { *raw.add(index) };
            if entry.is_null() {
                break;
            }

            collected.push(unsafe { CStr::from_ptr(entry) }.to_owned());
            index += 1;
        }

        Self(collected)
    }

    fn find(&self, prefix: &str) -> Option<usize> {
        self.0
            .iter()
            .position(|entry| entry.to_bytes().starts_with(prefix.as_bytes()))
    }

    /// Sets one `KEY=VALUE` entry, reporting whether the environment changed.
    fn set(&mut self, key: &str, value: &[u8]) -> bool {
        let prefix = format!("{key}=");
        let mut entry = prefix.clone().into_bytes();
        entry.extend_from_slice(value);

        let Ok(entry) = CString::new(entry) else {
            return false;
        };

        match self.find(&prefix) {
            Some(index) => match self.0.get_mut(index) {
                Some(slot) if *slot == entry => false,
                Some(slot) => {
                    *slot = entry;
                    true
                }
                None => false,
            },
            None => {
                self.0.push(entry);
                true
            }
        }
    }

    /// Adds `layer` to this environment's `LD_PRELOAD`, keeping whatever the
    /// caller put there.
    ///
    /// Returns `false` when the caller already preloads the layer, so the
    /// environment can be passed through untouched.
    fn merge_preload(&mut self, layer: &[u8]) -> bool {
        let prefix = format!("{LD_PRELOAD_ENV}=");
        let existing = self.find(&prefix);

        let current = existing
            .and_then(|index| self.0.get(index))
            .and_then(|entry| entry.to_bytes().strip_prefix(prefix.as_bytes()))
            .map(<[u8]>::to_vec)
            .unwrap_or_default();

        if current
            .split(|byte| *byte == b':' || byte.is_ascii_whitespace())
            .any(|entry| entry == layer)
        {
            return false;
        }

        let mut merged = prefix.into_bytes();
        merged.extend_from_slice(&current);
        if !current.is_empty() {
            merged.push(b':');
        }
        merged.extend_from_slice(layer);

        let Ok(entry) = CString::new(merged) else {
            return false;
        };

        match existing {
            Some(index) => {
                if let Some(slot) = self.0.get_mut(index) {
                    *slot = entry;
                }
            }
            None => self.0.push(entry),
        }

        true
    }

    /// Turns this into a null-terminated C list.
    ///
    /// The strings and the list itself are leaked: the callee keeps using them
    /// after this returns, and on a successful `exec` nothing in this image runs
    /// again to free them.
    fn leak(self) -> *const *const c_char {
        let mut list = self
            .0
            .into_iter()
            .map(|entry| entry.into_raw().cast_const())
            .collect::<Vec<_>>();

        list.push(ptr::null());

        Box::leak(list.into_boxed_slice()).as_ptr()
    }
}

/// Builds the environment to `exec` with, or [`None`] to use the caller's.
///
/// A path holding `:` or whitespace cannot be expressed in `LD_PRELOAD`, and the
/// caller's own environment is preferred over failing the `exec` it asked for.
unsafe fn prepare_envp(envp: *const *const c_char) -> Option<*const *const c_char> {
    let layer = std::env::var_os(REMOTE_LAYER_PATH_ENV)?;
    let layer = layer.as_os_str().as_bytes();

    if layer
        .iter()
        .any(|byte| *byte == b':' || byte.is_ascii_whitespace())
    {
        tracing::warn!(
            path = ?OsStr::from_bytes(layer),
            "remote-layer path cannot be used in LD_PRELOAD, layer will not survive exec"
        );
        return None;
    }

    let mut envp = unsafe { Envp::from_raw(envp) };
    let merged = envp.merge_preload(layer);
    // Carried whether or not the preload changed, so an image that already
    // inherits the layer can still rebuild the preload for its own children.
    let published = envp.set(REMOTE_LAYER_PATH_ENV, layer);

    if !merged && !published {
        return None;
    }

    tracing::trace!(path = ?OsStr::from_bytes(layer), "remote-layer preserving layer across exec");
    Some(envp.leak())
}

#[hook_fn]
unsafe extern "C" fn execve_detour(
    path: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    unsafe {
        let replacement = prepare_envp(envp);
        FN_EXECVE(path, argv, replacement.unwrap_or(envp))
    }
}

pub(crate) unsafe fn enable_exec_hooks(hook_manager: &mut HookManager) {
    unsafe {
        replace!(hook_manager, "execve", execve_detour, FnExecve, FN_EXECVE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(envp: &Envp) -> Vec<String> {
        envp.0
            .iter()
            .map(|entry| entry.to_str().unwrap().to_owned())
            .collect()
    }

    fn envp_of(entries: &[&str]) -> Envp {
        Envp(entries.iter().map(|e| CString::new(*e).unwrap()).collect())
    }

    #[test]
    fn adds_preload_when_the_caller_set_none() {
        let mut envp = envp_of(&["PATH=/usr/bin"]);
        assert!(envp.merge_preload(b"/tmp/mirrord/layer.so"));
        assert_eq!(
            entries(&envp),
            ["PATH=/usr/bin", "LD_PRELOAD=/tmp/mirrord/layer.so"]
        );
    }

    #[test]
    fn keeps_what_the_caller_already_preloads() {
        let mut envp = envp_of(&["LD_PRELOAD=/opt/other.so"]);
        assert!(envp.merge_preload(b"/tmp/mirrord/layer.so"));
        assert_eq!(
            entries(&envp),
            ["LD_PRELOAD=/opt/other.so:/tmp/mirrord/layer.so"]
        );
    }

    /// Nothing to do, so the caller's environment is passed through untouched.
    #[test]
    fn leaves_an_environment_that_already_has_the_layer() {
        let mut envp = envp_of(&["LD_PRELOAD=/opt/other.so:/tmp/mirrord/layer.so"]);
        assert!(!envp.merge_preload(b"/tmp/mirrord/layer.so"));
    }

    /// The new image rebuilds the preload for its own children from this, so it
    /// has to travel with the preload rather than be left behind.
    #[test]
    fn carries_the_layer_path_for_the_next_exec() {
        let mut envp = envp_of(&["PATH=/usr/bin"]);
        assert!(envp.set(REMOTE_LAYER_PATH_ENV, b"/tmp/mirrord/layer.so"));
        assert_eq!(
            entries(&envp),
            [
                "PATH=/usr/bin",
                "MIRRORD_REMOTE_LAYER_PATH=/tmp/mirrord/layer.so"
            ]
        );
    }

    #[test]
    fn replaces_a_stale_layer_path() {
        let mut envp = envp_of(&["MIRRORD_REMOTE_LAYER_PATH=/old/layer.so"]);
        assert!(envp.set(REMOTE_LAYER_PATH_ENV, b"/tmp/mirrord/layer.so"));
        assert_eq!(
            entries(&envp),
            ["MIRRORD_REMOTE_LAYER_PATH=/tmp/mirrord/layer.so"]
        );
    }

    /// Nothing to change, so the caller's environment can be passed through.
    #[test]
    fn reports_no_change_when_the_layer_path_already_matches() {
        let mut envp = envp_of(&["MIRRORD_REMOTE_LAYER_PATH=/tmp/mirrord/layer.so"]);
        assert!(!envp.set(REMOTE_LAYER_PATH_ENV, b"/tmp/mirrord/layer.so"));
    }

    #[test]
    fn does_not_confuse_a_similarly_named_variable() {
        let mut envp = envp_of(&["LD_PRELOAD_EXTRA=/opt/other.so"]);
        assert!(envp.merge_preload(b"/tmp/mirrord/layer.so"));
        assert_eq!(
            entries(&envp),
            [
                "LD_PRELOAD_EXTRA=/opt/other.so",
                "LD_PRELOAD=/tmp/mirrord/layer.so"
            ]
        );
    }
}
