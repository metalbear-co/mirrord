//! # mirrord Wizard (aka onboarding Wizard)
//!
//! `mirrord wizard` is a thin alias for `mirrord ui` that opens the browser directly on the config
//! wizard page (`/wizard`).
//!
//! The wizard's frontend and its backend endpoints are part of the shared `mirrord ui` server
//! (see [`crate::ui`] and `ui::wizard`); this command just starts that server if it isn't already
//! running and points the browser at the wizard page. The frontend itself lives in `packages/ui`
//! (composing `packages/wizard`).

use mirrord_analytics::{AnalyticsReporter, ExecutionKind};

use crate::{
    config::{UiArgs, WizardArgs},
    error::CliResult,
    ui,
    user_data::UserData,
};

/// The entrypoint for the `wizard` command. Starts the shared `mirrord ui` server (if needed) and
/// opens the browser on the wizard page.
pub async fn wizard_command(
    args: WizardArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    // The reporter fires a launch event on drop; `is-returning` is now tracked server-side by the
    // wizard's `cluster-details` endpoint once the user starts the config flow.
    let _analytics = AnalyticsReporter::new(
        args.telemetry,
        ExecutionKind::Wizard,
        watch,
        user_data.machine_id(),
        None,
    );

    ui::ui_command(
        UiArgs {
            port: args.port,
            no_browser: args.no_browser,
            command: None,
        },
        "/wizard",
    )
    .await?;

    Ok(())
}
