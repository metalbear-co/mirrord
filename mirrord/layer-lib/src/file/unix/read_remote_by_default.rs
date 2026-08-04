/// These paths will be read remotely by default when `fs.feature.mode` is set to
/// `localwithoverrides`.
pub const PATHS: [&str; 4] = [
    // for dns resolving
    r"^/etc/resolv.conf$",
    r"^/etc/hosts$",
    r"^/etc/hostname$",
    // CA bundle, so that the local process trusts the same authorities as the remote target when
    // talking to services in the cluster.
    r"^/etc/ssl/certs(/|$)",
];
