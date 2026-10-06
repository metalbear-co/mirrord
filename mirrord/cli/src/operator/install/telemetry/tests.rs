use mirrord_analytics::Analytics;
use serde_json::{Value, json};

use super::{Outcome, add_outcome};
use crate::operator::install::OperatorInstallError;

fn properties(result: Result<Outcome, &OperatorInstallError>, phase: u32) -> Value {
    let mut analytics = Analytics::default();
    add_outcome(&mut analytics, result, phase);
    serde_json::to_value(analytics).unwrap()
}

/// Each way a run ends has its own `outcome`, and only a run that failed or was interrupted
/// reports the `phase` that it ended in.
#[test]
fn outcome_and_phase() {
    assert_eq!(properties(Ok(Outcome::Success), 8), json!({ "outcome": 0 }));
    assert_eq!(
        properties(Ok(Outcome::NotInstalled), 3),
        json!({ "outcome": 3 })
    );
    assert_eq!(
        properties(Err(&OperatorInstallError::Declined), 5),
        json!({ "outcome": 2 })
    );
    assert_eq!(
        properties(Err(&OperatorInstallError::Interrupted), 8),
        json!({ "outcome": 4, "phase": 8 })
    );
    assert_eq!(
        properties(Err(&OperatorInstallError::SignupRateLimited), 6),
        json!({ "outcome": 1, "phase": 6 })
    );
}
