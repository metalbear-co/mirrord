//! The debug hold's hops. Each has its own variable and one reader, and nothing goes further.
//!
//! 1. [`MIRRORD_DEBUG_HOLD`]`=1`: `mirrord up --debug <svc>` asks that service's `mirrord exec` to
//!    hold the program it starts. Only `exec` reads it, and the program never gets it.
//! 2. That `exec` hands the hold on, by platform:
//!    - Windows: `exec` creates the program and stays its parent, so it holds the program itself,
//!      from outside. The program gets nothing.
//!    - Linux and macOS: `exec` replaces itself with the program through `execve`, so no `exec`
//!      code is left to hold it, and the program's own layer holds instead. The program gets
//!      [`MIRRORD_LAYER_DEBUG_HOLD`]`=<exec's pid>`, which `execve` keeps, so only that process
//!      matches.
//! 3. [`MIRRORD_DEBUG_DOTNET_PORT`]`=<address>`: `mirrord up` serves a .NET diagnostic port for
//!    each debugged service, and hands its address to that service's `exec`. `exec` merges it into
//!    the program's `DOTNET_DiagnosticPorts` ([`dotnet_ports::merge`]), which every .NET runtime
//!    in the program's tree inherits and pauses on at its startup. A child whose creator hands it
//!    an explicit environment gets the entry from the creator's layer instead
//!    ([`dotnet_ports::forwarded`]). The program never gets the mirrord variable itself.
//!
//! Two names rather than two values of one: a `mirrord exec` the program starts reads only
//! [`MIRRORD_DEBUG_HOLD`], so nothing the program's layer is told can make that `exec` hold too,
//! with no IDE waiting for it.

use std::{collections::HashMap, ffi::OsStr};

pub use crate::{MIRRORD_DEBUG_DOTNET_PORT, MIRRORD_DEBUG_HOLD, MIRRORD_LAYER_DEBUG_HOLD};

/// The value of [`MIRRORD_DEBUG_HOLD`] that asks for a hold. Any other is no request.
pub const REQUESTED: &str = "1";

/// Whether this process's environment carries `mirrord up`'s request.
pub fn requested_in_env() -> bool {
    std::env::var_os(MIRRORD_DEBUG_HOLD).is_some_and(|value| value == REQUESTED)
}

/// The pid a value of [`MIRRORD_LAYER_DEBUG_HOLD`] names: decimal digits only, anything else is
/// `None`.
pub fn layer_pid(value: &OsStr) -> Option<u32> {
    let value = value.to_str()?;
    // Digits only: `u32::from_str` also takes a leading `+`.
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

/// The .NET runtime's `DOTNET_DiagnosticPorts`, and mirrord's entry in it.
///
/// The value is `;`-separated entries, each `address[,tag]…`, with the tags `connect`, `listen`,
/// `suspend` and `nosuspend` (dotnet/diagnostics' `ipc-protocol.md`). The runtime reads one value,
/// and waits at its startup for a `ResumeRuntime` on every `suspend` port in it, so mirrord adds
/// its own entry and keeps the user's, rather than replacing them.
pub mod dotnet_ports {
    use super::HashMap;

    /// The variable the runtime reads its diagnostic ports from.
    pub const DOTNET_DIAGNOSTIC_PORTS: &str = "DOTNET_DiagnosticPorts";

    /// The older spelling of [`DOTNET_DIAGNOSTIC_PORTS`], which the runtime reads when that one
    /// isn't set.
    pub const COMPLUS_DIAGNOSTIC_PORTS: &str = "COMPlus_DiagnosticPorts";

    /// The variables that make a runtime pause on, or wait for, a diagnostic port. A remote pod's
    /// values name sockets inside the pod, which a local runtime would wait on forever.
    pub const REMOTE_DROPPED: &[&str] = &[
        DOTNET_DIAGNOSTIC_PORTS,
        COMPLUS_DIAGNOSTIC_PORTS,
        "DOTNET_DefaultDiagnosticPortSuspend",
        "COMPlus_DefaultDiagnosticPortSuspend",
    ];

    /// How `mirrord up` names its pipe on Windows: `mirrord-dotnet-<uuid>`. The runtime adds the
    /// `\\.\pipe\` itself.
    pub const PIPE_PREFIX: &str = "mirrord-dotnet-";

    /// The directory a Unix socket of `mirrord up`'s sits in, `mirrord-debug-<uid>`, starts with
    /// this.
    pub const SOCKET_DIR_PREFIX: &str = "mirrord-debug-";

    /// The name of a Unix socket of `mirrord up`'s, `dotnet-<up's pid>-<uuid>.sock`, starts with
    /// this.
    pub const SOCKET_PREFIX: &str = "dotnet-";

    /// The entry the runtime connects to `address` with, and pauses on until it's resumed.
    pub fn entry(address: &str) -> String {
        format!("{address},connect,suspend")
    }

    /// Whether an entry's address is one `mirrord up` serves: a pipe named `mirrord-dotnet-…`, or
    /// a socket `…/mirrord-debug-<uid>/dotnet-…`. Both shapes are checked on every platform, since
    /// neither can be mistaken for the other.
    pub fn is_mirrord_entry(entry: &str) -> bool {
        let address = entry.split(',').next().unwrap_or_default().trim();
        let pipe = address
            .get(..PIPE_PREFIX.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(PIPE_PREFIX));
        let socket = address.rsplit_once('/').is_some_and(|(dir, name)| {
            let dir = dir.rsplit_once('/').map_or(dir, |(_, last)| last);
            dir.starts_with(SOCKET_DIR_PREFIX) && name.starts_with(SOCKET_PREFIX)
        });
        pipe || socket
    }

    /// `value` without mirrord's entries and without empty ones, or `None` when nothing is left.
    ///
    /// A mirrord entry in a value `exec` inherits is stale: it comes from an outer `mirrord up`, a
    /// session that may be gone, and a `suspend` port nobody serves pauses the runtime forever.
    pub fn without_mirrord_entries(value: &str) -> Option<String> {
        let kept = value
            .split(';')
            .filter(|entry| !entry.trim().is_empty() && !is_mirrord_entry(entry))
            .collect::<Vec<_>>();
        (!kept.is_empty()).then(|| kept.join(";"))
    }

    /// The `DOTNET_DiagnosticPorts` that adds `address` to what the program would read.
    ///
    /// The runtime reads [`DOTNET_DIAGNOSTIC_PORTS`], or [`COMPLUS_DIAGNOSTIC_PORTS`] when that
    /// one isn't set. So the user's entries come from whichever the runtime would read, and the
    /// result is written to [`DOTNET_DIAGNOSTIC_PORTS`], where it hides the `COMPlus_` one. The
    /// user's entries stay first, verbatim and in order; stale mirrord entries and empty ones are
    /// dropped (see [`without_mirrord_entries`]), and ours goes last.
    ///
    /// # Arguments
    ///
    /// * `dotnet` - the program's [`DOTNET_DIAGNOSTIC_PORTS`], if set.
    /// * `complus` - the program's [`COMPLUS_DIAGNOSTIC_PORTS`], if set.
    /// * `address` - the port `mirrord up` serves for this service.
    pub fn merge(dotnet: Option<&str>, complus: Option<&str>, address: &str) -> String {
        let ours = entry(address);
        match dotnet.or(complus).and_then(without_mirrord_entries) {
            Some(theirs) => format!("{theirs};{ours}"),
            None => ours,
        }
    }

    /// The `DOTNET_DiagnosticPorts` a child created with an explicit environment gets, so its
    /// runtimes pause on the port its parent's pause on. `None` when there's nothing to change:
    /// the parent has no port of mirrord's, or the child's environment already carries it.
    ///
    /// A creator that builds its child's environment itself, as `dotnet run` does from a launch
    /// profile, can leave the variable out or set its own value, which replaces the inherited one.
    /// The child's runtime would then never pause on `mirrord up`'s port, and a debugger would
    /// miss its startup. So the parent's entry joins the child's ports by [`merge`]'s rules: the
    /// child's entries first, verbatim, and the parent's last.
    ///
    /// NOTE: an accepted cost. A creator that empties its child's environment on purpose, to start
    /// it clean, still gives the child mirrord's entry. The hooks can't tell that apart from a
    /// launch profile that leaves the variable out, and only the entry joins: every other variable
    /// stays as the creator wrote it.
    ///
    /// # Arguments
    ///
    /// * `parent_dotnet`, `parent_complus` - the creating process's [`DOTNET_DIAGNOSTIC_PORTS`]
    ///   and [`COMPLUS_DIAGNOSTIC_PORTS`], if set. Its own runtime reads the first one set.
    /// * `child_dotnet`, `child_complus` - the same two in the child's environment.
    pub fn forwarded(
        parent_dotnet: Option<&str>,
        parent_complus: Option<&str>,
        child_dotnet: Option<&str>,
        child_complus: Option<&str>,
    ) -> Option<String> {
        let address = |entry: &str| {
            entry
                .split(',')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let ours = parent_dotnet
            .or(parent_complus)?
            .split(';')
            .filter(|entry| is_mirrord_entry(entry))
            .map(address)
            .next_back()?;
        let carried = child_dotnet
            .or(child_complus)
            .is_some_and(|value| value.split(';').any(|entry| address(entry) == ours));
        (!carried).then(|| merge(child_dotnet, child_complus, &ours))
    }

    /// Drops [`REMOTE_DROPPED`] from a remote pod's environment, matching names without case so
    /// the drop holds wherever the program runs.
    pub fn drop_remote(env: &mut HashMap<String, String>) {
        env.retain(|name, _| {
            !REMOTE_DROPPED
                .iter()
                .any(|dropped| name.eq_ignore_ascii_case(dropped))
        });
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::{dotnet_ports::*, *};

    #[rstest]
    #[case::a_pid("4242", Some(4242))]
    // The odd one out: an `exec` running as pid 1, as in a container, is still a pid here, since
    // `mirrord up`'s request travels under another name.
    #[case::pid_one("1", Some(1))]
    #[case::signed("+42", None)]
    #[case::empty("", None)]
    #[case::anything_else("pid:42", None)]
    fn a_layer_pid_is_decimal_digits_only(#[case] value: &str, #[case] expected: Option<u32>) {
        assert_eq!(layer_pid(OsStr::new(value)), expected, "{value:?}");
    }

    const OURS: &str = "mirrord-dotnet-ab12";

    /// What the runtime gets, for each value the program would otherwise read. The user's
    /// entries stay first and verbatim, since their tools must still work, and ours goes last.
    #[rstest]
    #[case::unset(None, None, "mirrord-dotnet-ab12,connect,suspend")]
    #[case::a_tool_of_theirs(
        Some("my-tool,nosuspend"),
        None,
        "my-tool,nosuspend;mirrord-dotnet-ab12,connect,suspend"
    )]
    // A stale entry from an outer `mirrord up` would pause the runtime on a port nobody serves.
    #[case::a_stale_pipe_of_ours(
        Some("mirrord-dotnet-old,connect,suspend;my-tool"),
        None,
        "my-tool;mirrord-dotnet-ab12,connect,suspend"
    )]
    #[case::a_stale_socket_of_ours(
        Some("/tmp/mirrord-debug-501/dotnet-77-cd34.sock,connect,suspend;my-tool"),
        None,
        "my-tool;mirrord-dotnet-ab12,connect,suspend"
    )]
    #[case::a_trailing_separator(
        Some("my-tool;"),
        None,
        "my-tool;mirrord-dotnet-ab12,connect,suspend"
    )]
    #[case::only_stale_entries(
        Some("MIRRORD-DOTNET-old;;"),
        None,
        "mirrord-dotnet-ab12,connect,suspend"
    )]
    // The odd one out: the runtime reads the old spelling when the new one is unset, so its
    // entries move to the variable that's written, which hides it.
    #[case::only_the_complus_spelling(None, Some("x"), "x;mirrord-dotnet-ab12,connect,suspend")]
    #[case::both_spellings(
        Some("theirs"),
        Some("ignored"),
        "theirs;mirrord-dotnet-ab12,connect,suspend"
    )]
    fn our_port_joins_the_ports_the_runtime_would_read(
        #[case] dotnet: Option<&str>,
        #[case] complus: Option<&str>,
        #[case] expected: &str,
    ) {
        assert_eq!(merge(dotnet, complus, OURS), expected);
    }

    /// A socket only counts as ours inside a `mirrord-debug-<uid>` directory: a user's own socket
    /// that happens to be named `dotnet-…` stays.
    #[rstest]
    #[case::our_pipe("mirrord-dotnet-ab12,connect", Owner::Mirrord)]
    #[case::our_socket("/run/user/1000/mirrord-debug-1000/dotnet-9-ab.sock", Owner::Mirrord)]
    #[case::their_socket_named_alike("/tmp/diag/dotnet-9-ab.sock", Owner::Theirs)]
    #[case::their_pipe("dotnet-diagnostic-1234", Owner::Theirs)]
    fn only_mirrords_own_addresses_are_its_entries(#[case] entry: &str, #[case] owner: Owner) {
        assert_eq!(
            is_mirrord_entry(entry),
            owner == Owner::Mirrord,
            "{entry} is {owner:?}'s"
        );
    }

    /// Whose diagnostic-port entry an address is.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Owner {
        /// A port `mirrord up` serves.
        Mirrord,
        /// Anything else, such as a tool of the user's.
        Theirs,
    }

    const PARENTS: &str = "my-tool;mirrord-dotnet-ab12,connect,suspend";

    /// What a child created with an explicit environment gets, for each value of its own. It
    /// keeps its entries, and gains the parent's port only when it doesn't carry it already.
    #[rstest]
    #[case::none_of_its_own(None, None, Some("mirrord-dotnet-ab12,connect,suspend"))]
    #[case::its_own_value(
        Some("theirs,listen,nosuspend"),
        None,
        Some("theirs,listen,nosuspend;mirrord-dotnet-ab12,connect,suspend")
    )]
    #[case::already_ours(Some("theirs;mirrord-dotnet-ab12,connect,suspend"), None, None)]
    // The runtime reads the old spelling when the new one is unset, so its entries move to the
    // variable that's written, as `merge` does.
    #[case::the_complus_spelling(
        None,
        Some("theirs"),
        Some("theirs;mirrord-dotnet-ab12,connect,suspend")
    )]
    // The odd one out: a port of another `mirrord up` is stale, and gives way to the parent's.
    #[case::a_stale_port_of_ours(
        Some("mirrord-dotnet-old,connect,suspend"),
        None,
        Some("mirrord-dotnet-ab12,connect,suspend")
    )]
    fn a_child_with_its_own_environment_gets_the_parents_port(
        #[case] child_dotnet: Option<&str>,
        #[case] child_complus: Option<&str>,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(
            forwarded(Some(PARENTS), None, child_dotnet, child_complus).as_deref(),
            expected
        );
    }

    /// Only a parent whose runtime reads a port of mirrord's hands one on, whichever spelling it
    /// reads.
    #[rstest]
    #[case::no_ports(None, None, None)]
    #[case::only_its_own_ports(Some("my-tool"), None, None)]
    #[case::ours_in_the_complus_spelling(
        None,
        Some(PARENTS),
        Some("mirrord-dotnet-ab12,connect,suspend")
    )]
    #[case::ours_hidden_by_the_dotnet_spelling(Some("my-tool"), Some(PARENTS), None)]
    fn only_a_parent_with_our_port_forwards_it(
        #[case] parent_dotnet: Option<&str>,
        #[case] parent_complus: Option<&str>,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(
            forwarded(parent_dotnet, parent_complus, None, None).as_deref(),
            expected
        );
    }

    /// A pod's diagnostic ports name sockets inside the pod: a local runtime would pause on them
    /// forever. Everything else of the pod's environment stays.
    #[test]
    fn a_pods_diagnostic_ports_are_dropped_and_nothing_else() {
        let mut env = HashMap::from([
            (
                "DOTNET_DiagnosticPorts".to_owned(),
                "/diag/dotnet-monitor.sock".to_owned(),
            ),
            ("complus_diagnosticports".to_owned(), "x".to_owned()),
            (
                "DOTNET_DefaultDiagnosticPortSuspend".to_owned(),
                "1".to_owned(),
            ),
            ("DOTNET_gcServer".to_owned(), "1".to_owned()),
        ]);

        drop_remote(&mut env);

        assert_eq!(
            env,
            HashMap::from([("DOTNET_gcServer".to_owned(), "1".to_owned())])
        );
    }
}
