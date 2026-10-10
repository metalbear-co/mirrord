use std::{ops::Not, sync::OnceLock};

use caps::{CapSet, Capability};
use mirrord_agent_env::mesh::MeshVendor;

use crate::{IPTablesWrapper, mesh::MeshVendorExt};

/// Returns correct [`IPTablesWrapper`] to use for traffic redirection.
///
/// If `nftables` is `false`, this function will return the `ip[6]tables-legacy` wrapper.
///
/// If `nftables` is `true`, this function will return the `ip[6]tables-nft` wrapper.
///
/// If `nftables` is [`None`], this function will choose between legacy and nftables:
/// 1. If mesh rules are found with `ip[6]tables-nft`, `ip[6]tables-nft` wrapper will be returned.
/// 2. Otherwise, if mesh rules are found with `ip[6]tables-legacy`, `ip[6]tables-legacy` wrapper
///    will be returned.
/// 3. Otherwise, if `ip[6]tables-legacy` is functional (checked by adding a dummy rule into the
///    "nat" table), `ip[6]tables-legacy` wrapper will be returned.
/// 4. Otherwise, `ip[6]tables-nft` wrapper will be returned.
///
/// # Safety
///
/// Unless `nftables` argument is provided, this function will use iptables commands when detecting
/// the correct backend to use. Such actions can automatically load backend-specific kernel
/// modules, and switch the active iptables backend in the kernel, at least in some cases. This is
/// **not** acceptable for us.
///
/// 1. In properly isolated Kubernetes implementations (unlike kind, for example), unprivileged
///    agents will never be able to load kernel modules. This is because the agent container does
///    not have [`Capability::CAP_SYS_MODULE`].
/// 2. Even in properly isolated Kubernetes implementations, privileged agents might still be able
///    to load kernel modules. Because of this, this function will drop the
///    [`Capability::CAP_SYS_MODULE`] before running any iptables commands.
pub fn get_iptables(nftables: Option<bool>, ip6: bool) -> IPTablesWrapper {
    /// Whether we should use ip6tables-nft when no backend is explicitly configured,
    ///
    /// Initialized with the first call of this function for IPv6, if the
    /// `nftables` argument is not provided.
    static DETECTED_NFTABLES_V6: OnceLock<bool> = OnceLock::new();
    /// Whether we should use iptables-nft when no backend is explicitly configured,
    ///
    /// Initialized with the first call of this function for IPv4, if the
    /// `nftables` argument is not provided.
    static DETECTED_NFTABLES_V4: OnceLock<bool> = OnceLock::new();
    let detected_nftables = if ip6 {
        &DETECTED_NFTABLES_V6
    } else {
        &DETECTED_NFTABLES_V4
    };

    // If `nftables` or `detected_nftables` is set, always return early.
    // This function calls itself recursively later.
    let nftables = nftables.or_else(|| detected_nftables.get().copied());
    if let Some(nftables) = nftables {
        let path = match (nftables, ip6) {
            (true, true) => "/usr/sbin/ip6tables-nft",
            (true, false) => "/usr/sbin/iptables-nft",
            (false, true) => "/usr/sbin/ip6tables-legacy",
            (false, false) => "/usr/sbin/iptables-legacy",
        };
        return iptables::new_with_cmd(path)
            .expect("IPTables initialization should not fail, the binary should be present in the agent image")
            .into();
    }

    let legacy = get_iptables(Some(false), ip6);
    let nft = get_iptables(Some(true), ip6);

    try_drop_cap_sys_module();

    for (backend, is_nft) in [(&legacy, false), (&nft, true)] {
        match MeshVendor::detect(backend) {
            Ok(Some(mesh)) => {
                tracing::info!(target: "mirrord_agent_iptables",
                    %mesh,
                    command = backend.tables.cmd,
                    "Detected mesh rules with one of the iptables backends. \
                    Using this backend."
                );
                let _ = detected_nftables.set(is_nft);
                return backend.clone();
            }
            Ok(None) => {
                tracing::debug!(target: "mirrord_agent_iptables",
                    command = backend.tables.cmd,
                    "No mesh rules detected with one of the iptables backends."
                );
            }
            Err(error) => {
                tracing::debug!(target: "mirrord_agent_iptables",
                    error = %error,
                    command = backend.tables.cmd,
                    "Failed to detect mesh rules with one of the iptables backends."
                );
            }
        }
    }

    let legacy_works = legacy
        .tables
        .append("nat", "INPUT", "-p 255 -j RETURN")
        .inspect_err(|error| {
            tracing::debug!(target: "mirrord_agent_iptables",
                %error,
                command = legacy.tables.cmd,
                "Failed to add a dummy rule using iptables-legacy binary, \
                assuming no kernel support for it.",
            )
        })
        .is_ok();
    let _ = detected_nftables.set(legacy_works.not());
    let wrapper = if legacy_works {
        let _ = legacy.tables.delete("nat", "INPUT", "-p 255 -j RETURN")
            .inspect_err(|error| {
                tracing::error!(target: "mirrord_agent_iptables",
                    %error,
                    command = legacy.tables.cmd,
                    "Failed to delete a dummy rule added when checking kernel support for iptables-legacy. \
                    The rule should not affect any traffic."
                )
            });
        legacy
    } else {
        nft
    };

    tracing::info!(target: "mirrord_agent_iptables",
        command = wrapper.tables.cmd,
        "Using iptables backend picked based on kernel support for iptables-legacy.",
    );

    wrapper
}

/// Checks whether an explicitly configured iptables backend hides service mesh rules living in the
/// other backend, and logs a loud warning if so.
///
/// When no backend is explicitly configured, [`get_iptables`] picks the backend where mesh rules
/// are found, so mesh-aware redirection kicks in automatically. An explicit setting skips that
/// detection. If the mesh's rules live in the other backend, mesh detection silently fails and the
/// agent falls back to the standard redirect, which races the mesh's own PREROUTING redirect and
/// can deliver still-encrypted mesh traffic (e.g. a raw TLS ClientHello) directly to the
/// application's plaintext port.
pub fn warn_on_backend_mesh_mismatch(nftables: bool, ip6: bool) {
    let selected = get_iptables(Some(nftables), ip6);
    if matches!(MeshVendor::detect(&selected), Ok(Some(..))) {
        return;
    }

    try_drop_cap_sys_module();

    let other = get_iptables(Some(nftables.not()), ip6);
    if let Ok(Some(mesh)) = MeshVendor::detect(&other) {
        tracing::warn!(target: "mirrord_agent_iptables",
            %mesh,
            configured_backend = selected.tables.cmd,
            mesh_rules_found_with = other.tables.cmd,
            "Service mesh rules were found with the iptables backend other than the explicitly \
            configured one. Mesh-aware traffic redirection will not be used, and intercepting \
            incoming traffic is likely to break meshed traffic to the target, e.g. deliver \
            encrypted bytes directly to the application's port. Remove the explicit backend \
            setting (`agent.nftables` config / `MIRRORD_AGENT_NFTABLES`) to let the agent pick \
            the backend automatically."
        );
    }
}

/// Drops [`Capability::CAP_SYS_MODULE`] from the current thread.
///
/// This will prevent the thread from loading kernel modules.
fn try_drop_cap_sys_module() {
    let has_cap = caps::has_cap(None, CapSet::Effective, Capability::CAP_SYS_MODULE)
        .inspect_err(|error| tracing::warn!(target: "mirrord_agent_iptables",%error, "Failed to check if the current thread has CAP_SYS_MODULE."))
        .unwrap_or_default();
    if has_cap.not() {
        tracing::debug!(target: "mirrord_agent_iptables","Verified that the current thread does not have CAP_SYS_MODULE.");
        return;
    }

    if let Err(error) = caps::drop(None, CapSet::Effective, Capability::CAP_SYS_MODULE) {
        tracing::error!(target: "mirrord_agent_iptables",%error, "Failed to drop CAP_SYS_MODULE from the current thread.");
    } else {
        tracing::debug!(target: "mirrord_agent_iptables","Dropped CAP_SYS_MODULE from the current thread.");
    }
}
