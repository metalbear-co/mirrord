Added a Clippy lint to disallow calling `.to_string()` on a `&str` - use `.to_owned()` instead. This
was voted on internally, and implemented mostly for the sake of consistency. See also
[this relevant bug](https://github.com/rust-lang/rust-clippy/issues/16569) in the current rust version.
