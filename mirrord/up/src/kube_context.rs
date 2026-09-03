use std::sync::Arc;

/// The kube context for this run of `mirrord up`. Can be used to calculate the kube context for
/// each target when resolving config or creating service configs and is cheaply clonable because it
/// contains `Arc`s.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct UpKubeContext {
    pub command_arg: Option<Arc<str>>,
    pub common_context: Option<Arc<str>>,
    pub user_default_context: Option<Arc<str>>,
}

impl UpKubeContext {
    // Decide with kube context to use for the given target
    pub fn get_context(&self, target_context: Option<Arc<str>>) -> Option<Arc<str>> {
        self.command_arg
            .as_ref()
            .or(target_context.as_ref())
            .or(self.common_context.as_ref())
            .or(self.user_default_context.as_ref())
            .cloned()
    }
}
