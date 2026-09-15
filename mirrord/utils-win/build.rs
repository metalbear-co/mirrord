//! Embeds the crash-dialog logo as a Windows bitmap resource.
//!
//! `embed-resource` no-ops on non-Windows targets, so this is safe to call unconditionally.

fn main() {
    embed_resource::compile("src/diagnostics/logo.rc", embed_resource::NONE);
}
