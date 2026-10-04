//! Tests for [`Envp`], which [`super::hooks`] uses to edit the environment of `execve`.
//!
//! They live here because the tests of `mirrord-layer-core` don't run.

use std::{
    ffi::{CString, c_char},
    ptr,
};

use mirrord_layer_core::envp::{Envp, entries};

/// A C copy of an environment, which stays valid while the test reads the list built from it.
struct Raw {
    _strings: Vec<CString>,
    pointers: Vec<*const c_char>,
}

impl Raw {
    fn new(env: &[&str]) -> Self {
        let strings = env
            .iter()
            .map(|entry| CString::new(*entry).unwrap())
            .collect::<Vec<_>>();
        let mut pointers = strings
            .iter()
            .map(|entry| entry.as_ptr())
            .collect::<Vec<_>>();
        pointers.push(ptr::null());

        Self {
            _strings: strings,
            pointers,
        }
    }
}

/// Applies `edit` to an [`Envp`] over a C copy of `env`, or over a null `envp` for [`None`], and
/// reads the result back. [`None`] means that the caller's list is used as is.
fn run(env: Option<&[&str]>, edit: impl FnOnce(&mut Envp<'_>)) -> Option<Vec<String>> {
    let raw = Raw::new(env.unwrap_or_default());
    let envp = if env.is_some() {
        raw.pointers.as_ptr()
    } else {
        ptr::null()
    };

    let mut envp = unsafe { Envp::from_raw(envp) };
    edit(&mut envp);
    let result = envp.into_raw()?;

    Some(
        unsafe { entries(result) }
            .map(|entry| entry.to_str().unwrap().to_owned())
            .collect(),
    )
}

fn set(key: &'static str, value: &'static str) -> impl FnOnce(&mut Envp<'_>) {
    move |envp| envp.set(key, value.as_bytes())
}

#[test]
fn keeps_caller_list_when_value_is_already_set() {
    assert_eq!(run(Some(&["A=1", "K=v", "B=2"]), set("K", "v")), None);
}

#[test]
fn appends_missing_key() {
    assert_eq!(
        run(Some(&["A=1"]), set("K", "v")),
        Some(vec!["A=1".to_owned(), "K=v".to_owned()])
    );
}

#[test]
fn replaces_first_entry_and_drops_later_duplicates() {
    assert_eq!(
        run(Some(&["K=old", "A=1", "K=older"]), set("K", "v")),
        Some(vec!["K=v".to_owned(), "A=1".to_owned()])
    );
}

#[test]
fn does_not_match_longer_key() {
    assert_eq!(
        run(Some(&["KEY=1"]), set("K", "v")),
        Some(vec!["KEY=1".to_owned(), "K=v".to_owned()])
    );
}

#[test]
fn null_envp_gets_only_the_new_entry() {
    assert_eq!(run(None, set("K", "v")), Some(vec!["K=v".to_owned()]));
}

#[test]
fn value_with_nul_keeps_caller_list() {
    assert_eq!(run(Some(&["A=1"]), set("K", "v\0")), None);
}

/// Only changed entries are allocated, the rest point at the caller's own strings.
#[test]
fn reuses_the_callers_strings() {
    let raw = Raw::new(&["A=1"]);
    let mut envp = unsafe { Envp::from_raw(raw.pointers.as_ptr()) };
    envp.set("K", b"v");
    let result = envp.into_raw().unwrap();

    assert_eq!(Some(unsafe { *result }), raw.pointers.first().copied());
}

#[test]
fn removes_every_entry_for_a_key() {
    assert_eq!(
        run(Some(&["K=old", "A=1", "K=older"]), |envp| envp.remove("K")),
        Some(vec!["A=1".to_owned()])
    );
}

#[test]
fn removing_a_missing_key_keeps_caller_list() {
    assert_eq!(run(Some(&["A=1"]), |envp| envp.remove("K")), None);
}

/// Removing a key the caller doesn't have still drops a value set for it earlier.
#[test]
fn removes_a_key_set_earlier() {
    assert_eq!(
        run(Some(&["A=1"]), |envp| {
            envp.set("K", b"v");
            envp.remove("K");
        }),
        None
    );
}

#[test]
fn keeps_the_last_value_set_for_a_key() {
    assert_eq!(
        run(Some(&["A=1"]), |envp| {
            envp.set("K", b"first");
            envp.set("K", b"second");
        }),
        Some(vec!["A=1".to_owned(), "K=second".to_owned()])
    );
}

/// Setting a key back to the caller's value leaves nothing to change.
#[test]
fn setting_the_callers_value_again_keeps_caller_list() {
    assert_eq!(
        run(Some(&["K=v"]), |envp| {
            envp.set("K", b"other");
            envp.set("K", b"v");
        }),
        None
    );
}

/// A value that can't be an entry leaves the earlier change to its key in place.
#[test]
fn keeps_the_earlier_value_over_one_with_a_nul() {
    assert_eq!(
        run(Some(&["A=1"]), |envp| {
            envp.set("K", b"v");
            envp.set("K", b"v\0");
        }),
        Some(vec!["A=1".to_owned(), "K=v".to_owned()])
    );
}

#[test]
fn applies_changes_to_several_keys() {
    assert_eq!(
        run(Some(&["A=1", "B=2", "C=3"]), |envp| {
            envp.set("B", b"two");
            envp.remove("C");
            envp.set("D", b"4");
        }),
        Some(vec!["A=1".to_owned(), "B=two".to_owned(), "D=4".to_owned()])
    );
}

/// Reads the value the caller has for a key, as the remote layer does to merge `LD_PRELOAD`.
#[test]
fn gets_the_callers_first_value() {
    let raw = Raw::new(&["K=first", "K=second"]);
    let envp = unsafe { Envp::from_raw(raw.pointers.as_ptr()) };

    assert_eq!(envp.get("K"), Some(&b"first"[..]));
    assert_eq!(envp.get("MISSING"), None);
}
