/// These paths will be read remotely by default when `fs.feature.mode` is set to
/// `localwithoverrides`.
pub const PATHS: [&str; 3] = [
    // for dns resolving
    r"^/etc/resolv.conf$",
    r"^/etc/hosts$",
    r"^/etc/hostname$",
];
