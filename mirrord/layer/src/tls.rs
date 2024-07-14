use libc::c_void;
use mirrord_layer_macro::hook_guard_fn;

use crate::{hooks::HookManager, replace};

// https://developer.apple.com/documentation/security/2980705-sectrustevaluatewitherror
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn sec_trust_evaluate_with_error_detour(
    trust: *const c_void,
    error: *const c_void,
) -> bool {
    tracing::trace!("sec_trust_evaluate_with_error_detour called");
    true
}

pub(crate) unsafe fn enable_tls_hooks(hook_manager: &mut HookManager) {
    replace!(
        hook_manager,
        "SecTrustEvaluateWithError",
        sec_trust_evaluate_with_error_detour,
        FnSec_trust_evaluate_with_error,
        FN_SEC_TRUST_EVALUATE_WITH_ERROR
    );
}
