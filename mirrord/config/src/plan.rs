//! The mirrord plans (<https://metalbear.com/mirrord/pricing/>), which every option of the config
//! records the one of in the schema, so tools such as `mirrord mcp` can tell an agent which plan an
//! option needs. An enum rather than a string, so that a typo in a plan doesn't build.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Schema annotation naming the [`Plan`] an option needs. Options without it inherit it from the
/// closest annotated option above them.
///
/// Set with `#[config(plan = Team)]` on fields of `MirrordConfig` types, and with
/// `#[schemars(extend("x-mirrord-plan" = crate::plan::Plan::Team))]` elsewhere.
pub const PLAN_ANNOTATION: &str = "x-mirrord-plan";

/// A mirrord plan, cheapest first.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Plan {
    /// Free, open source mirrord.
    Oss,
    /// mirrord for Teams, which comes with the mirrord Operator.
    Team,
    Enterprise,
}
