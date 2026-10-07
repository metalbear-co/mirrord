//! Environment variables as Windows sees them.
//!
//! Windows treats environment names case-insensitively: `Path` and `PATH` are one variable. It
//! compares them ordinally, code unit by code unit over UTF-16, after mapping each unit through
//! the system's uppercase table, and keeps an environment block sorted in that order.
//! [`windows_name_order`] makes exactly that comparison. Rust's `to_uppercase` is not a substitute:
//! it works on `char`s and can expand one into several (`ß` becomes `SS`), which the system table
//! never does.
//!
//! [`WindowsEnv`] is keyed by [`EnvName`], which orders by [`windows_name_order`], so a child can
//! never receive both `Path` and `PATH`, and the block it gets is already in the order Windows
//! expects.

use std::{cmp::Ordering, collections::BTreeMap};

use str_win::{MultiBufferChar, multi_buffer_ptr_to_strings};
use winapi::{shared::minwindef::TRUE, um::stringapiset::CompareStringOrdinal};

// `CompareStringOrdinal` results, from `winnls.h`. The winapi crate does not define them.
const CSTR_LESS_THAN: i32 = 1;
const CSTR_EQUAL: i32 = 2;
const CSTR_GREATER_THAN: i32 = 3;

/// Orders two environment names the way Windows orders the entries of an environment block:
/// ordinally over UTF-16, ignoring case through the system's uppercase table.
///
/// `=C:` sorts before every letter, because `=` is below them, and `_` after every letter, because
/// it is compared against their uppercase forms.
///
/// Names that are ASCII as far as they differ are compared here: the system table maps ASCII
/// letters to their ASCII uppercase and leaves every other ASCII unit alone. Anything else asks the
/// system.
///
/// # Arguments
///
/// * `left`, `right` - the names, as UTF-16 code units without a terminator.
pub fn windows_name_order(left: &[u16], right: &[u16]) -> Ordering {
    ascii_name_order(left, right).unwrap_or_else(|| system_name_order(left, right))
}

/// [`windows_name_order`] for names whose units are ASCII up to the first difference.
///
/// # Returns
///
/// `None` as soon as a pair of units holds anything but ASCII.
fn ascii_name_order(left: &[u16], right: &[u16]) -> Option<Ordering> {
    let upper = |unit: u16| match unit {
        0x61..=0x7A => unit - 0x20,
        _ => unit,
    };
    for (&left_unit, &right_unit) in left.iter().zip(right) {
        if left_unit > 0x7F || right_unit > 0x7F {
            return None;
        }
        match upper(left_unit).cmp(&upper(right_unit)) {
            Ordering::Equal => {}
            unequal => return Some(unequal),
        }
    }
    // One name is a prefix of the other, and an ordinal comparison puts the shorter first.
    Some(left.len().cmp(&right.len()))
}

/// [`windows_name_order`], answered by `CompareStringOrdinal`.
fn system_name_order(left: &[u16], right: &[u16]) -> Ordering {
    let (Ok(left_len), Ok(right_len)) = (i32::try_from(left.len()), i32::try_from(right.len()))
    else {
        return left.cmp(right);
    };

    match unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, TRUE) }
    {
        CSTR_LESS_THAN => Ordering::Less,
        CSTR_EQUAL => Ordering::Equal,
        CSTR_GREATER_THAN => Ordering::Greater,
        // Fails only on invalid arguments, which two slices cannot be.
        _ => left.cmp(right),
    }
}

/// An environment variable name, equal to every other casing of itself.
///
/// Keeps the name both as text and as UTF-16, so it is encoded once rather than on every
/// comparison.
#[derive(Clone, Debug)]
pub struct EnvName {
    wide: Vec<u16>,
    text: String,
}

impl EnvName {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            wide: text.encode_utf16().collect(),
            text,
        }
    }

    /// The name, in the casing it was given.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl Ord for EnvName {
    fn cmp(&self, other: &Self) -> Ordering {
        windows_name_order(&self.wide, &other.wide)
    }
}

impl PartialOrd for EnvName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for EnvName {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for EnvName {}

/// The environment a child process receives, one entry per variable, in block order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowsEnv(BTreeMap<EnvName, String>);

impl WindowsEnv {
    pub fn new() -> Self {
        Self::default()
    }

    /// This process's environment.
    ///
    /// Where one name appears in more than one casing, the first entry wins, as in
    /// [`WindowsEnv::from_ordered_entries`].
    ///
    /// A name or value that is not valid UTF-16 is converted lossily. `std::env::vars` would
    /// panic on it instead, and this runs inside the target's `CreateProcess` hook, where a panic
    /// ends the process.
    pub fn inherited() -> Self {
        Self::from_ordered_entries(std::env::vars_os().map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        }))
    }

    /// Builds an environment from entries in the order a block holds them.
    ///
    /// Where one name appears in more than one casing, the first entry wins, which is the one
    /// `GetEnvironmentVariable` finds, because it scans the block from the start.
    pub fn from_ordered_entries(entries: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut environment = BTreeMap::new();
        for (name, value) in entries {
            environment.entry(EnvName::new(name)).or_insert(value);
        }
        Self(environment)
    }

    /// The value of `name`, under any casing.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(&EnvName::new(name)).map(String::as_str)
    }

    /// Sets `name` to `value`, replacing the variable under any casing it already has. `name`
    /// keeps the casing given here.
    pub fn set(&mut self, name: &str, value: String) {
        let name = EnvName::new(name);
        // `insert` on an existing key keeps the old key, and with it the old casing.
        self.0.remove(&name);
        self.0.insert(name, value);
    }

    /// Removes `name` under any casing, returning its value.
    pub fn remove(&mut self, name: &str) -> Option<String> {
        self.0.remove(&EnvName::new(name))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The variables, in block order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// Reads an environment block, the inverse of [`WindowsEnv::to_block`]: `u16` units for a
    /// Unicode block, `u8` for an ANSI one.
    ///
    /// The block is read up to the empty string that ends it and no further: nothing after it
    /// belongs to the block's owner, and it may not even be readable. A name that appears more
    /// than once keeps its first entry, as in [`WindowsEnv::from_ordered_entries`], and an entry
    /// without a name and a `=` is dropped (see [`valid_entries`]). The edge cases follow
    /// <https://nullprogram.com/blog/2023/08/23/>.
    ///
    /// # Safety
    ///
    /// `block` must point to a readable, properly aligned block that is ended by an empty string.
    pub unsafe fn from_block<T: MultiBufferChar>(block: *const T) -> Self {
        let encoding = if size_of::<T>() == size_of::<u16>() {
            "Unicode"
        } else {
            "ANSI"
        };
        Self::from_ordered_entries(valid_entries(
            unsafe { multi_buffer_ptr_to_strings(block) },
            encoding,
        ))
    }

    /// Builds the UTF-16 block `CreateProcessW` takes with `CREATE_UNICODE_ENVIRONMENT`.
    ///
    /// Entries come out in the order Windows hands them out and the C runtime expects back. Each
    /// one is `name=value\0`, and the block ends with one more `\0`. An empty environment is
    /// therefore `\0\0`: a lone `\0` is not a valid block.
    pub fn to_block(&self) -> Vec<u16> {
        // A value takes no more UTF-16 units than it takes UTF-8 bytes, so this is an upper bound.
        let capacity = self
            .0
            .iter()
            .map(|(name, value)| name.wide.len() + value.len() + 2)
            .sum::<usize>()
            + 2;
        let mut block = Vec::with_capacity(capacity);
        for (name, value) in &self.0 {
            block.extend_from_slice(&name.wide);
            block.push(u16::from(b'='));
            block.extend(value.encode_utf16());
            block.push(0);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        block
    }
}

/// Splits environment strings into names and values, dropping the invalid ones.
///
/// An entry is valid when it carries a non-empty name before a `=`.
///
/// An entry that starts with `=` is valid and must be kept. Windows records the current directory
/// of each drive in the environment block, as `=C:=C:/work`, and the C runtime reads them back to
/// resolve a drive-relative path. Dropping them changes where a child process resolves such a path.
///
/// Only the number of dropped entries is logged, never their text: an entry that lacks its `=` may
/// still be a secret.
///
/// # Arguments
///
/// * `encoding` - the block's encoding, for the log.
fn valid_entries(strings: Vec<String>, encoding: &'static str) -> Vec<(String, String)> {
    let total = strings.len();
    let entries = strings
        .into_iter()
        .filter_map(|entry| {
            let name_len = environment_name_of(&entry)?.len();
            let value = entry[name_len + 1..].to_owned();
            let mut name = entry;
            name.truncate(name_len);
            Some((name, value))
        })
        .collect::<Vec<_>>();

    let dropped = total - entries.len();
    if dropped > 0 {
        tracing::warn!(
            dropped,
            total,
            encoding,
            "dropped environment entries without a name and a `=`"
        );
    }

    entries
}

/// Returns the name part of an environment entry, without the `=` that ends it.
///
/// A leading `=` belongs to the name, not to the separator. Windows records the current directory
/// of a drive as `=C:=C:/work`, whose name is `=C:`.
///
/// # Arguments
///
/// * `entry` - one `name=value` entry from an environment block.
///
/// # Returns
///
/// The name, or `None` when the entry has no separator or an empty name.
fn environment_name_of(entry: &str) -> Option<&str> {
    let start = usize::from(entry.starts_with('='));
    let separator = entry[start..].find('=')? + start;

    (separator > start).then(|| &entry[..separator])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    #[test]
    fn names_compare_like_windows() {
        assert_eq!(windows_name_order(&wide("é"), &wide("É")), Ordering::Equal);
        assert_ne!(windows_name_order(&wide("ß"), &wide("SS")), Ordering::Equal);
        assert_eq!(
            windows_name_order(&wide("path"), &wide("PATH")),
            Ordering::Equal
        );
        assert_eq!(windows_name_order(&wide("=C:"), &wide("A")), Ordering::Less);
    }

    /// The ASCII comparison answers exactly what the system does, punctuation included: `[`, `_`
    /// and `^` sit between the uppercase and lowercase letters, so they sort after every letter.
    #[test]
    fn the_ascii_order_is_the_system_order() {
        let a = u16::from(b'a');
        let x = u16::from(b'x');
        let upper_x = u16::from(b'X');
        for left in 1..=0x7F_u16 {
            for right in 1..=0x7F_u16 {
                for (left, right) in [
                    (vec![left], vec![right]),
                    (vec![left, a], vec![right]),
                    (vec![x, left], vec![upper_x, right, right]),
                ] {
                    assert_eq!(
                        ascii_name_order(&left, &right),
                        Some(system_name_order(&left, &right)),
                        "{left:?} vs {right:?}"
                    );
                }
            }
        }
        assert_eq!(ascii_name_order(&wide("aé"), &wide("aÉ")), None);
        assert_eq!(
            ascii_name_order(&wide("ab"), &wide("aé")),
            None,
            "a non-ASCII unit at the first difference asks the system"
        );
    }

    #[test]
    fn names_order_like_windows() {
        let environment = WindowsEnv::from_ordered_entries(
            [
                "path",
                "=C:",
                "Windir",
                "_underscore",
                "=D:",
                "A",
                "PATH2",
                "É",
                "e",
            ]
            .map(|name| (name.to_owned(), String::new())),
        );
        let names = environment.iter().map(|(name, _)| name).collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "=C:",
                "=D:",
                "A",
                "e",
                "path",
                "PATH2",
                "Windir",
                "_underscore",
                "É"
            ]
        );
    }

    #[test]
    fn the_first_entry_of_a_name_wins() {
        let environment = WindowsEnv::from_ordered_entries([
            ("Path".to_owned(), "first".to_owned()),
            ("OTHER".to_owned(), "x".to_owned()),
            ("PATH".to_owned(), "second".to_owned()),
            ("path".to_owned(), "third".to_owned()),
        ]);
        assert_eq!(
            environment.iter().collect::<Vec<_>>(),
            [("OTHER", "x"), ("Path", "first")]
        );
        assert_eq!(environment.get("pAtH"), Some("first"));
    }

    #[test]
    fn set_replaces_every_casing_and_keeps_the_new_one() {
        let mut environment = WindowsEnv::from_ordered_entries([
            ("Path".to_owned(), "a".to_owned()),
            ("x".to_owned(), "1".to_owned()),
        ]);
        environment.set("PATH", "b".to_owned());
        assert_eq!(
            environment.iter().collect::<Vec<_>>(),
            [("PATH", "b"), ("x", "1")]
        );

        assert_eq!(environment.remove("path"), Some("b".to_owned()));
        assert_eq!(environment.iter().collect::<Vec<_>>(), [("x", "1")]);
    }

    #[test]
    fn block_is_sorted_and_terminated() {
        let environment = WindowsEnv::from_ordered_entries([
            ("b".to_owned(), "2".to_owned()),
            ("=C:".to_owned(), "C:\\work".to_owned()),
            ("A".to_owned(), "é".to_owned()),
        ]);
        assert_eq!(
            String::from_utf16(&environment.to_block()).unwrap(),
            "=C:=C:\\work\0A=é\0b=2\0\0"
        );
    }

    #[test]
    fn empty_block_is_two_terminators() {
        assert_eq!(WindowsEnv::new().to_block(), vec![0, 0]);
    }

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

    /// Reads `entries` back from a Unicode block that holds them in the given order.
    fn from_entries(entries: &[&str]) -> WindowsEnv {
        unsafe { WindowsEnv::from_block(unicode_block(entries).as_ptr()) }
    }

    #[test]
    fn a_block_reads_back_in_block_order() {
        assert_eq!(
            from_entries(&["PATH=C:\\bin", "USER=test"])
                .iter()
                .collect::<Vec<_>>(),
            [("PATH", "C:\\bin"), ("USER", "test")]
        );

        let environment = WindowsEnv::from_ordered_entries([
            ("b".to_owned(), "2".to_owned()),
            ("=C:".to_owned(), "C:\\work".to_owned()),
            ("A".to_owned(), "é".to_owned()),
        ]);
        assert_eq!(
            unsafe { WindowsEnv::from_block(environment.to_block().as_ptr()) },
            environment,
            "the inverse of `to_block`"
        );
    }

    #[test]
    fn an_ansi_value_that_is_not_utf8_is_kept() {
        let block = [b'V', b'A', b'R', b'=', 0xFF, 0xFE, 0, 0];
        let environment = unsafe { WindowsEnv::from_block(block.as_ptr()) };
        assert_eq!(environment.get("VAR"), Some("\u{FFFD}\u{FFFD}"));
    }

    #[test]
    fn keeps_the_current_directory_of_each_drive() {
        let environment = from_entries(&[
            "=C:=C:/work",
            "=D:=D:/",
            "=ExitCode=00000000",
            "PATH=C:/Windows",
        ]);

        assert_eq!(environment.get("=C:"), Some("C:/work"));
        assert_eq!(environment.get("=D:"), Some("D:/"));
        assert_eq!(environment.get("=ExitCode"), Some("00000000"));
        assert_eq!(environment.get("PATH"), Some("C:/Windows"));
    }

    /// An entry needs a name and a `=` after it. A leading `=` belongs to the name, so `=INVALID`
    /// is a name with no separator after it, and `==value` is the empty name `=`.
    #[test]
    fn drops_entries_without_a_name() {
        let entries = [
            "",
            "no-separator",
            "=",
            "==value",
            "=INVALID",
            "PATH=C:/Windows",
        ]
        .map(str::to_owned)
        .to_vec();

        assert_eq!(
            valid_entries(entries, "Unicode"),
            [("PATH".to_owned(), "C:/Windows".to_owned())]
        );
    }

    #[test]
    fn keeps_an_empty_value() {
        assert_eq!(from_entries(&["EMPTY="]).get("EMPTY"), Some(""));
    }

    #[test]
    fn keeps_a_value_that_holds_separators() {
        assert_eq!(
            from_entries(&["PATH=C:/a;C:/b=c"]).get("PATH"),
            Some("C:/a;C:/b=c")
        );
    }

    #[test]
    fn an_empty_block_is_an_empty_environment() {
        assert_eq!(from_entries(&[]), WindowsEnv::new());
        assert_eq!(
            unsafe { WindowsEnv::from_block([0u8, 0].as_ptr()) },
            WindowsEnv::new()
        );
    }
}
