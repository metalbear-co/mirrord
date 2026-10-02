//! Edits the environment passed to `execve` without copying the caller's strings.
//!
//! An `execve` hook can't free what it allocates: `execve` reads the list after the hook returns,
//! and a successful `exec` never returns to free it. Under `posix_spawn`, glibc runs `execve` in a
//! `vfork` child that shares the parent's memory, so every allocation lands in the parent, once per
//! spawn. [`Envp`] hands back the caller's list when nothing changes, and otherwise allocates only
//! the changed entries and the pointer list.

// That `vfork` child runs on a stack of about 32 KiB, where an overflow crashes the application's
// child process. This lint flags any function here whose frame exceeds `stack-size-threshold`,
// which the workspace's `.clippy.toml` lowers to 4 KiB.
#![warn(clippy::large_stack_frames)]

use std::{
    ffi::{CStr, CString, c_char},
    marker::PhantomData,
    ptr,
};

/// A view of the caller's `envp`, plus the entries changed in it.
///
/// The caller's strings are never copied: an `envp` that needs no change is handed to `execve`
/// as is, and otherwise only the changed entries are allocated, next to pointers to the caller's
/// own strings.
pub struct Envp<'a> {
    raw: *const *const c_char,
    /// Entries replacing the caller's value for their key, or added when the caller has none.
    /// [`None`] removes the key.
    ///
    /// A [`Vec`] rather than a map: only a handful of fixed keys are ever changed, each once, so
    /// a linear scan is cheaper than hashing and keeps appended entries in insertion order. Worth
    /// revisiting if the set of keys grows large or becomes dynamic.
    changes: Vec<(&'static str, Option<CString>)>,
    _caller: PhantomData<&'a CStr>,
}

impl<'a> Envp<'a> {
    /// # Safety
    ///
    /// `raw` must be null, or a null-terminated array of valid C strings that outlive `'a`.
    pub unsafe fn from_raw(raw: *const *const c_char) -> Self {
        Self {
            raw,
            changes: Vec::new(),
            _caller: PhantomData,
        }
    }

    fn entries(&self) -> impl Iterator<Item = &'a CStr> + use<'a> {
        // SAFETY: upheld by the caller of `from_raw`.
        unsafe { entries(self.raw) }
    }

    /// Returns the caller's value for `key`, taking the first entry like `getenv` does.
    pub fn get(&self, key: &str) -> Option<&'a [u8]> {
        self.entries().find_map(|entry| value_of(entry, key))
    }

    /// Sets `key` to `value`, unless the caller's environment already has it.
    pub fn set(&mut self, key: &'static str, value: &[u8]) {
        if self.get(key) == Some(value) {
            return;
        }

        if let Ok(entry) = CString::new([key.as_bytes(), b"=", value].concat()) {
            self.changes.push((key, Some(entry)));
        }
    }

    /// Removes every entry for `key` from the caller's environment.
    pub fn remove(&mut self, key: &'static str) {
        if self.get(key).is_some() {
            self.changes.push((key, None));
        }
    }

    /// Builds the list to `exec` with, or [`None`] when nothing changed and the caller's list can
    /// be used as is.
    ///
    /// A changed entry takes the place of the caller's first entry for its key, and later
    /// duplicates are dropped so the new image can't read a stale one. Entries the caller didn't
    /// have are appended. A removed key loses all its entries.
    ///
    /// The changed strings and the list are leaked: `execve` reads them after this returns, and a
    /// successful `exec` never returns to free them. After a `vfork`, as in `posix_spawn`, they
    /// land in the parent's memory, once per spawn that needed a change.
    pub fn into_raw(self) -> Option<*const *const c_char> {
        if self.changes.is_empty() {
            return None;
        }

        let entries = self.entries();
        let mut changes = self.changes;

        let mut list = Vec::new();
        for entry in entries {
            match changes
                .iter_mut()
                .find(|(key, _)| value_of(entry, key).is_some())
            {
                Some((_, change)) => {
                    list.extend(change.take().map(|change| change.into_raw().cast_const()))
                }
                None => list.push(entry.as_ptr()),
            }
        }
        list.extend(
            changes
                .into_iter()
                .filter_map(|(_, change)| change)
                .map(|change| change.into_raw().cast_const()),
        );
        list.push(ptr::null());

        Some(Box::leak(list.into_boxed_slice()).as_ptr())
    }
}

/// Returns the value of `entry` when it is `key=value`.
fn value_of<'e>(entry: &'e CStr, key: &str) -> Option<&'e [u8]> {
    entry
        .to_bytes()
        .strip_prefix(key.as_bytes())?
        .strip_prefix(b"=")
}

/// Iterates a null-terminated C array of strings without copying it.
///
/// # Safety
///
/// `raw` must be null, or a null-terminated array of valid C strings that outlive `'a`.
pub unsafe fn entries<'a>(raw: *const *const c_char) -> impl Iterator<Item = &'a CStr> {
    (0..).map_while(move |index| {
        if raw.is_null() {
            return None;
        }

        let entry = unsafe { *raw.add(index) };
        (!entry.is_null()).then(|| unsafe { CStr::from_ptr(entry) })
    })
}
