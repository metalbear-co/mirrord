//! Building a Windows `lpCommandLine` string from an argv vector.
//!
//! When mirrord launches a target process via `CreateProcessW`, it passes the
//! resolved executable as `lpApplicationName` and a separate command line as
//! `lpCommandLine`. `CreateProcessW` does no parsing of `lpCommandLine` when
//! `lpApplicationName` is set, but the child's own CRT (and anything using
//! `CommandLineToArgvW`) still tokenises `lpCommandLine` to populate `argv`.
//!
//! That tokenisation splits on whitespace, so any argument containing a space
//! (e.g. an executable under `C:\Program Files\...`) must be quoted, or the
//! child sees a corrupted `argv`. These helpers apply CRT-compatible quoting so
//! the parser on the other side reconstructs the original arguments exactly.

/// Build a Windows command line from an executable path plus arguments.
///
/// `exe` becomes `argv[0]` (the convention is that the module name is repeated
/// as the first token; some applications rely on this, and it costs nothing to
/// be correct). Each entry is quoted as needed via [`push_quoted`].
pub fn build_command_line(exe: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(exe.len() + 2);
    push_quoted(&mut out, exe);
    for arg in args {
        out.push(' ');
        push_quoted(&mut out, arg);
    }
    out
}

/// Append `arg` to `out`, quoted according to the rules documented for
/// `CommandLineToArgvW` / the MSVC CRT parser.
///
/// Runs of backslashes followed by a literal double quote have to be doubled,
/// and the quote itself escaped, so that the parser on the other side
/// reconstructs the original string exactly. A bare arg with no whitespace or
/// quotes is passed through verbatim; this keeps the resulting command line
/// readable in the common case.
fn push_quoted(out: &mut String, arg: &str) {
    let needs_quoting = arg.is_empty() || arg.chars().any(|c| matches!(c, ' ' | '\t' | '"' | '\n'));
    if !needs_quoting {
        out.push_str(arg);
        return;
    }

    out.push('"');
    let mut iter = arg.chars().peekable();
    while let Some(c) = iter.next() {
        match c {
            '\\' => {
                let mut backslashes: usize = 1;
                while iter.peek() == Some(&'\\') {
                    iter.next();
                    backslashes += 1;
                }
                match iter.peek() {
                    None => {
                        for _ in 0..backslashes * 2 {
                            out.push('\\');
                        }
                    }
                    Some(&'"') => {
                        for _ in 0..backslashes * 2 + 1 {
                            out.push('\\');
                        }
                        out.push('"');
                        iter.next();
                    }
                    Some(_) => {
                        for _ in 0..backslashes {
                            out.push('\\');
                        }
                    }
                }
            }
            '"' => {
                out.push('\\');
                out.push('"');
            }
            other => out.push(other),
        }
    }
    out.push('"');
}
