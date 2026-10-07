//! Early diagnostic snapshot for the Windows layer.
//!
//! This is the thin layer-win side of the crash diagnostics. The heavy lifting lives in
//! `utils-win`. Here we only gather the snapshot and log it.
//!
//! The snapshot records who this process is, its session role, and what is loaded into it. A
//! flagged security product is logged at WARN. So the next failure carries that context in the log
//! even when no crash handler ever fires.
//!
//! It comes in two parts. [`log_loader_snapshot`] runs inside `DllMain` and only names the process
//! and its role. [`log_process_context`] runs on the startup worker once the layer has reported
//! ready: it captures the module table, once, for both the log and the crash handler, and opens
//! every loaded module's file for its vendor. None of that is needed to start the target, so none
//! of it holds the target up.

use std::{fmt::Write as _, net::SocketAddr, path::Path};

use mirrord_config::MIRRORD_LAYER_CRASH_MONITOR_ADDR;
use mirrord_layer_lib::{
    logging::current_log_file,
    process::windows::diagnostics::{SessionRole, session_role},
};
use utils_win::{
    diagnostics::{
        crash::{self, InstallOptions},
        crash_dir, crash_reporting_enabled, full_memory_dump,
        monitor::{InitReport, Registration, report_init_failure_for},
    },
    modules::flag_security_modules,
    process::{ProcessIdentity, get_current_process_name},
};

use crate::process::get_export;

/// Readies the in-process crash handler to produce records, and registers with the monitor.
///
/// The filter itself is already in place: `DllMain` installs it before it enables the hooks (see
/// `hooks::initialize_hooks`). This is the part that must not run under the loader lock, kept to
/// what a crash before readiness needs; the module table waits for [`log_process_context`].
///
/// Artifacts go to the resolved crash directory (the layer log directory, or a mirrord temp
/// directory when no log path is set). When a crash monitor endpoint is set, this also registers
/// for out-of-process dumps.
pub fn register_crash_monitor() {
    if !crash_reporting_enabled() {
        tracing::debug!("crash reporting disabled for extension-managed process");
        return;
    }

    let directory = crash_dir();

    let process_name = get_current_process_name();
    let role = session_role();
    let monitor = monitor_registration(&role, &process_name, &directory);

    crash::register_monitor(InstallOptions {
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
///
/// # Arguments
///
/// * `role` - this process's session role.
/// * `process_name` - this process's image name.
/// * `directory` - the session directory, which is the monitor's too.
pub(crate) fn monitor_registration(
    role: &SessionRole,
    process_name: &str,
    directory: &Path,
) -> Option<(SocketAddr, Registration)> {
    let address = monitor_address()?;

    let registration = Registration {
        pid: std::process::id(),
        parent_pid: parent_pid(role),
        name: process_name.to_owned(),
        role: role.label().to_owned(),
        // Filled in by `crash::register_monitor`, which owns the incident stem.
        stem: String::new(),
        // The exact log file, so the monitor bundles this one and not a prior session's reused pid.
        // The logger and the monitor read the directory from the same variable, so only the name
        // is new to the monitor. That holds only for an absolute directory: a relative one names
        // a different directory in this process's working directory than in the monitor's. The
        // CLI makes the session's directory absolute, so a relative one comes from a launch
        // outside it, and its log is not registered rather than looked for in the wrong place.
        log_name: current_log_file()
            .filter(|path| directory.is_absolute() && path.parent() == Some(directory))
            .and_then(|path| path.file_name()?.to_str().map(str::to_owned)),
        init_report: None,
    };

    Some((address, registration))
}

/// The session's crash monitor, when one is configured.
fn monitor_address() -> Option<SocketAddr> {
    std::env::var(MIRRORD_LAYER_CRASH_MONITOR_ADDR)
        .ok()?
        .parse()
        .ok()
}

/// This process's parent in the report tree. `0` for a top-level process, where the monitor
/// already knows the root CLI pid.
fn parent_pid(role: &SessionRole) -> u32 {
    match role {
        SessionRole::Child { parent_pid, .. } => *parent_pid,
        _ => 0,
    }
}

/// Tells the out-of-process monitor that layer initialization failed.
///
/// The layer calls this when its asynchronous startup fails or panics, so the monitor surfaces a
/// report instead of seeing a silent clean exit. A layer that registered signals through its crash
/// channel. One that failed before it registered sends the failure in a registration of its own,
/// the way a launcher reports on a child's behalf.
pub fn signal_init_failure(reason: &str) {
    if crash::signal_init_failure(reason) || !crash_reporting_enabled() {
        return;
    }
    let Some(address) = monitor_address() else {
        return;
    };
    report_init_failure_for(
        address,
        std::process::id(),
        parent_pid(&session_role()),
        InitReport::Failed(format!(
            "The layer's startup failed, and the process ended: {reason}"
        )),
    );
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
    ("kernel32", "GetProcAddress"),
    ("kernelbase", "SetUnhandledExceptionFilter"),
    ("ntdll", "NtCreateFile"),
    ("ntdll", "NtReadFile"),
    ("ntdll", "NtWriteFile"),
    ("ntdll", "NtClose"),
    ("ws2_32", "connect"),
    ("ws2_32", "WSAConnect"),
    ("ws2_32", "getaddrinfo"),
];

/// Logs the part of the start-up snapshot that is safe under the loader lock: the pid and the
/// session role.
pub fn log_loader_snapshot() {
    let role = session_role();

    tracing::info!(
        pid = std::process::id(),
        role = role.label(),
        "layer early snapshot",
    );

    if let SessionRole::MalformedEnv { detail } = &role {
        tracing::warn!("session role is malformed-env: {detail}");
    }
}

/// Logs the part of the start-up snapshot that waits until the layer reported ready.
///
/// It records the parent, the integrity level and session, the module inventory (captured once,
/// shared with the crash handler, and logged as one event), and any security product among the
/// loaded modules (a version-resource read per module file). Flagged security modules are logged
/// at WARN.
pub fn log_process_context() {
    let identity = ProcessIdentity::capture();

    tracing::info!(
        pid = identity.pid,
        parent_pid = ?identity.parent_pid,
        parent = identity.parent_name.as_deref().unwrap_or("?"),
        integrity = identity.integrity,
        session = ?identity.session_id,
        wow64 = identity.wow64,
        "layer process context",
    );

    let table = crash::capture_modules();
    let inventory = table
        .entries()
        .iter()
        .fold(String::new(), |mut inventory, entry| {
            let _ = write!(
                inventory,
                "\n  {:#018x} {:>10} {}",
                entry.base,
                entry.size,
                entry.path.display()
            );
            inventory
        });
    tracing::info!(modules = table.len(), "module inventory:{inventory}");

    let flagged = flag_security_modules(table);
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
