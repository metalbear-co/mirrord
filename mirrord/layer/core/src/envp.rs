//! Edits the environment passed to `execve` without copying the caller's strings.
//!
//! An `execve` hook can't free what it allocates: `execve` reads the list after the hook returns,
//! and a successful `exec` never returns to free it. Under `posix_spawn`, glibc runs `execve` in a
//! `vfork` child that shares the parent's memory, so every allocation lands in the parent, once per
//! spawn. [`Envp`] hands back the caller's list when nothing changes, and otherwise allocates only
//! the changed entries and the pointer list.

use std::{
    ffi::{CStr, CString, c_char},
    marker::PhantomData,
    ptr,
};

/// A pending change to one key of the caller's environment.
enum Change {
    /// The entry that replaces the caller's entries for the key, or is appended when it has none.
    Set(CString),
    /// Drops every caller's entry for the key.
    Remove,
}

/// A view of the caller's `envp`, plus the entries changed in it.
///
/// The caller's strings are never copied: an `envp` that needs no change is handed to `execve`
/// as is, and otherwise only the changed entries are allocated, next to pointers to the caller's
/// own strings.
pub struct Envp<'a> {
    raw: *const *const c_char,
    /// At most one change per key, the latest one made.
    ///
    /// A [`Vec`] rather than a map: only a handful of fixed keys are ever changed, so a linear
    /// scan is cheaper than hashing and keeps appended entries in insertion order.
    changes: Vec<(&'static str, Change)>,
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
    ///
    /// Replaces any earlier [`Envp::set`] or [`Envp::remove`] of `key`. Ignored, keeping any
    /// earlier change, when `value` contains a NUL byte, which can't be part of an entry.
    pub fn set(&mut self, key: &'static str, value: &[u8]) {
        if value.contains(&0) {
            return;
        }

        self.forget(key);
        if self.get(key) == Some(value) {
            return;
        }

        if let Ok(entry) = CString::new([key.as_bytes(), b"=", value].concat()) {
            self.changes.push((key, Change::Set(entry)));
        }
    }

    /// Removes every entry for `key` from the caller's environment.
    ///
    /// Replaces any earlier [`Envp::set`] or [`Envp::remove`] of `key`.
    pub fn remove(&mut self, key: &'static str) {
        self.forget(key);
        if self.get(key).is_some() {
            self.changes.push((key, Change::Remove));
        }
    }

    /// Drops the pending change to `key`.
    fn forget(&mut self, key: &str) {
        self.changes.retain(|(changed, _)| *changed != key);
    }

    /// Builds the list to `exec` with, or [`None`] when nothing changed and the caller's list can
    /// be used as is.
    ///
    /// A set entry takes the place of the caller's first entry for its key, and later duplicates
    /// are dropped so the new image can't read a stale one. Entries the caller didn't have are
    /// appended. A removed key loses all its entries.
    ///
    /// The set entries and the list are leaked: `execve` reads them after this returns, and a
    /// successful `exec` never returns to free them. After a `vfork`, as in `posix_spawn`, they
    /// land in the parent's memory, once per spawn that needed a change.
    pub fn into_raw(self) -> Option<*const *const c_char> {
        if self.changes.is_empty() {
            return None;
        }

        let entries = self.entries();
        // Each change, with whether it has already taken the place of the caller's first entry.
        let mut changes = self
            .changes
            .into_iter()
            .map(|(key, change)| (key, change, false))
            .collect::<Vec<_>>();

        let mut list = Vec::new();
        for entry in entries {
            match changes
                .iter_mut()
                .find(|(key, ..)| value_of(entry, key).is_some())
            {
                None => list.push(entry.as_ptr()),
                Some((_, _, true)) => {}
                Some((_, change, placed)) => {
                    *placed = true;
                    if let Change::Set(set) = change {
                        list.push(set.as_ptr());
                    }
                }
            }
        }
        list.extend(changes.iter().filter(|(_, _, placed)| !placed).filter_map(
            |(_, change, _)| match change {
                Change::Set(set) => Some(set.as_ptr()),
                Change::Remove => None,
            },
        ));
        list.push(ptr::null());

        // Every set entry is in the list, in place of the caller's entry or appended.
        for (_, change, _) in changes {
            if let Change::Set(set) = change {
                std::mem::forget(set);
            }
        }

        Some(list.leak().as_ptr())
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
