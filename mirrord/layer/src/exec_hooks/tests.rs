use std::{
    ffi::{CStr, CString},
    ptr,
};

use super::with_env;

/// Calls [`with_env`] with a C copy of `env`, or with a null `envp` for [`None`], and reads the
/// result back. [`None`] means that `with_env` kept the caller's list.
fn run(env: Option<&[&str]>, key: &str, value: &str) -> Option<Vec<String>> {
    let strings = env
        .unwrap_or_default()
        .iter()
        .map(|entry| CString::new(*entry).unwrap())
        .collect::<Vec<_>>();
    let mut pointers = strings
        .iter()
        .map(|entry| entry.as_ptr())
        .collect::<Vec<_>>();
    pointers.push(ptr::null());
    let envp = if env.is_some() {
        pointers.as_ptr()
    } else {
        ptr::null()
    };

    let result = unsafe { with_env(envp, key, value) }?;
    let entries = (0..)
        .map(|index| unsafe { *result.add(index) })
        .take_while(|entry| !entry.is_null())
        .map(|entry| {
            unsafe { CStr::from_ptr(entry) }
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect();

    Some(entries)
}

#[test]
fn keeps_caller_list_when_value_is_already_set() {
    assert_eq!(run(Some(&["A=1", "K=v", "B=2"]), "K", "v"), None);
}

#[test]
fn appends_missing_key() {
    assert_eq!(
        run(Some(&["A=1"]), "K", "v"),
        Some(vec!["A=1".to_owned(), "K=v".to_owned()])
    );
}

#[test]
fn replaces_first_entry_and_drops_later_duplicates() {
    assert_eq!(
        run(Some(&["K=old", "A=1", "K=older"]), "K", "v"),
        Some(vec!["K=v".to_owned(), "A=1".to_owned()])
    );
}

#[test]
fn does_not_match_longer_key() {
    assert_eq!(
        run(Some(&["KEY=1"]), "K", "v"),
        Some(vec!["KEY=1".to_owned(), "K=v".to_owned()])
    );
}

#[test]
fn null_envp_gets_only_the_new_entry() {
    assert_eq!(run(None, "K", "v"), Some(vec!["K=v".to_owned()]));
}

#[test]
fn value_with_nul_keeps_caller_list() {
    assert_eq!(run(Some(&["A=1"]), "K", "v\0"), None);
}
