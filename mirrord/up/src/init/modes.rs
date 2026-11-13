//! How both wizards present the modes. The `mirrord ui` server sends these to
//! the GUI.

use std::ops::Not;

use mirrord_config::target::TargetType;

use crate::{INFERRED_TARGET_TYPES, config::ServiceMode};

impl ServiceMode {
    /// The modes offered, in order. The first is preselected.
    pub const OFFERED: [Self; 3] = [Self::Split, Self::Mirror, Self::Replace];

    /// The name shown in the mode question.
    pub fn label(self) -> &'static str {
        match self {
            Self::Split => "Split",
            Self::Mirror => "Mirror",
            Self::Replace => "Replace",
        }
    }

    /// Shown under the name in the mode question.
    pub fn description(self) -> &'static str {
        match self {
            Self::Split => {
                "Your requests reach your local process. Everyone else's keep hitting the deployed service, so you can work in a shared cluster without disrupting anyone."
            }
            Self::Mirror => {
                "A copy of your session's traffic is sent to your local process. The deployed service still handles every request, including yours."
            }
            Self::Replace => {
                "Every request reaches your local process. The deployed workload is scaled to zero for the session and restored when it ends."
            }
        }
    }

    /// Marked as recommended in the mode question.
    pub fn is_recommended(self) -> bool {
        matches!(self, Self::Split)
    }

    /// The kinds of workload that the target question lists for this mode.
    ///
    /// `replace` copies the workload and scales the original down. Of the
    /// kinds that a service name can resolve to, `mirrord up` allows that only
    /// for these (see `validate_targets`). A replicaset, which the operator can
    /// also scale down through its owner, can still be written in the YAML by
    /// hand. The question does not list replicasets, because a deployment keeps
    /// its old ones, and their names change with every rollout.
    pub fn target_kinds(self) -> &'static [TargetType] {
        match self {
            Self::Replace => &[TargetType::Deployment, TargetType::StatefulSet],
            Self::Split | Self::Mirror => &INFERRED_TARGET_TYPES,
        }
    }

    /// Whether the target question offers to run without a target. Without a
    /// target there is no incoming traffic to split, mirror or replace, so only
    /// `split` offers it.
    pub fn allows_targetless(self) -> bool {
        matches!(self, Self::Split)
    }

    /// `replace` takes all the traffic, so it has no HTTP filter.
    pub fn uses_http_filter(self) -> bool {
        matches!(self, Self::Replace).not()
    }

    /// The yes/no question before the traffic questions.
    pub fn traffic_question(self) -> &'static str {
        if self.uses_http_filter() {
            "Customize the header filter and ports?"
        } else {
            "Customize ports?"
        }
    }
}
