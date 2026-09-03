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

use std::{
    ffi::{CStr, CString, OsStr, c_char},
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    ptr,
};

use libc::c_int;
use mirrord_layer_core::{hooks::HookManager, replace};
use mirrord_layer_macro::hook_fn;

const LD_PRELOAD_ENV: &str = "LD_PRELOAD";

/// Path this shared object was loaded from, as the dynamic loader records it.
///
/// The bootstrap chooses where to extract the layer, so the path is not known at
/// build time. Asking the loader avoids having to publish it through a second
/// environment variable that would then have to survive the same `exec` this
/// module exists to handle.
fn layer_path() -> Option<PathBuf> {
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();

    // SAFETY: `layer_path` is a symbol in this object, so the loader can resolve
    // it; `dladdr` only writes through the pointer when it returns non-zero.
    let info = unsafe {
        if libc::dladdr(layer_path as *const libc::c_void, info.as_mut_ptr()) == 0 {
            return None;
        }
        info.assume_init()
    };

    if info.dli_fname.is_null() {
        return None;
    }

    // SAFETY: non-null on success, and owned by the loader for the life of the
    // process.
    let path = unsafe { CStr::from_ptr(info.dli_fname) };
    Some(PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}

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
    let layer = layer_path()?;
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
    if !envp.merge_preload(layer) {
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
