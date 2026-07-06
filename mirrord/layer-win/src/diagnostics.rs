//! Early diagnostic snapshot for the Windows layer.
//!
//! This is the thin layer-win side of the crash diagnostics. The heavy lifting lives in
//! `utils-win`. Here we only gather the snapshot and log it.
//!
//! The snapshot records who this process is, its session role, and what is loaded into it. A
//! flagged security product is logged at WARN. So the next failure carries that context in the log
//! even when no crash handler ever fires.

use std::net::SocketAddr;

use mirrord_config::MIRRORD_LAYER_CRASH_MONITOR_ADDR;
use mirrord_layer_lib::{
    logging::current_log_file,
    process::windows::diagnostics::{SessionRole, session_role},
};
use utils_win::{
    diagnostics::{
        crash::{self, InstallOptions},
        crash_dir, full_memory_dump,
        monitor::Registration,
    },
    modules::{ModuleTable, flag_security_modules},
    process::{ProcessIdentity, get_current_process_name},
};

use crate::process::get_export;

/// Installs the in-process crash handler.
///
/// Artifacts go to the resolved crash directory (the layer log directory, or a mirrord temp
/// directory when no log path is set). When a crash monitor endpoint is set, this also registers
/// for out-of-process dumps.
pub fn install_crash_handler() {
    let directory = crash_dir();

    let process_name = get_current_process_name();
    let role = session_role();
    let monitor = monitor_registration(&role, &process_name);

    crash::install(InstallOptions {
        directory,
        process_name,
        full_memory: full_memory_dump(),
        monitor,
    });
}

/// Builds the crash-monitor endpoint and this process's registration, when a monitor is configured.
///
/// The parent pid comes from the session role. It is `0` for a top-level process, where the monitor
/// already knows the root CLI pid.
fn monitor_registration(
    role: &SessionRole,
    process_name: &str,
) -> Option<(SocketAddr, Registration)> {
    let address = std::env::var(MIRRORD_LAYER_CRASH_MONITOR_ADDR)
        .ok()?
        .parse::<SocketAddr>()
        .ok()?;

    let parent_pid = match role {
        SessionRole::Child { parent_pid, .. } => *parent_pid,
        _ => 0,
    };

    let registration = Registration {
        pid: std::process::id(),
        parent_pid,
        name: process_name.to_owned(),
        role: role.label().to_owned(),
        // Filled in by `crash::install`, which owns the incident stem.
        stem: String::new(),
        // The exact log file, so the monitor bundles this one and not a prior session's reused pid.
        log_path: current_log_file().map(|path| path.to_string_lossy().into_owned()),
    };

    Some((address, registration))
}

/// Signals the out-of-process monitor that layer initialization failed.
///
/// The layer calls this when `layer_start` errors (e.g. a `for_child` miss because the parent died
/// and the init event was deleted), so the monitor surfaces a report instead of seeing a silent
/// clean exit.
pub fn signal_init_failure(reason: &str) {
    crash::signal_init_failure(reason);
}

/// Reserves crash-handler stack on the current thread.
///
/// Called on every `DLL_THREAD_ATTACH` so a stack overflow on a worker thread still has room to run
/// the handler. A no-op until the crash handler is installed.
pub fn reserve_handler_stack() {
    crash::reserve_handler_stack();
}

/// Removes the in-process crash handler.
///
/// # Arguments
///
/// * `process_terminating` - whether the process is exiting, not just unloading the layer.
pub fn uninstall_crash_handler(process_terminating: bool) {
    crash::uninstall(process_terminating);
}

/// The functions we hook. Their prologues are snapshotted before we touch them.
const HOOKED_FUNCTIONS: &[(&str, &str)] = &[
    ("kernelbase", "CreateProcessInternalW"),
    ("kernel32", "LoadLibraryW"),
    ("ntdll", "NtCreateFile"),
];

/// Logs a one-time snapshot of the process at layer start.
///
/// It records identity, session role, and a one-line module summary followed by the full inventory.
/// Flagged security modules are logged at WARN.
pub fn log_early_snapshot() {
    let identity = ProcessIdentity::capture();
    let role = session_role();

    tracing::info!(
        pid = identity.pid,
        parent_pid = ?identity.parent_pid,
        parent = identity.parent_name.as_deref().unwrap_or("?"),
        role = role.label(),
        integrity = identity.integrity,
        session = ?identity.session_id,
        wow64 = identity.wow64,
        cmdline = %identity.command_line,
        "layer early snapshot",
    );

    if let SessionRole::MalformedEnv { detail } = &role {
        tracing::warn!("session role is malformed-env: {detail}");
    }

    log_module_summary();
}

/// Logs the module count plus any flagged security vendors.
fn log_module_summary() {
    let table = ModuleTable::capture();
    let flagged = flag_security_modules(&table);

    if flagged.is_empty() {
        tracing::info!(modules = table.len(), "module summary: no flagged vendors");
    } else {
        let vendors = flagged
            .iter()
            .map(|module| format!("{} ({})", module.name, module.vendor))
            .collect::<Vec<_>>()
            .join(", ");
        tracing::warn!(
            modules = table.len(),
            "module summary: flagged security modules present: {vendors}",
        );
    }

    for entry in table.entries() {
        tracing::info!(
            base = format!("{:#018x}", entry.base),
            size = entry.size,
            "module: {}",
            entry.path.display(),
        );
    }
}

/// Logs the first bytes of the functions we hook, before we hook them.
///
/// A foreign prologue is an existing hook. An EDR shows up this way. This runs only under the
/// `prologues` toggle.
pub fn log_prologues() {
    for (module, function) in HOOKED_FUNCTIONS {
        let address = get_export(module, function);
        if address.is_null() {
            tracing::warn!("prologue: {module}!{function} not found");
            continue;
        }

        let bytes = unsafe { std::slice::from_raw_parts(address as *const u8, 16) };
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        tracing::info!("prologue: {module}!{function} {hex}");
    }
}
