use std::io::{self, IsTerminal, Write};

use mirrord_analytics::{AnalyticsReporter, ReportTarget, Reporter};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_sip::rosetta::{
    RosettaFallbackEntry, acquire_rosetta_fallback_report_lock, load_rosetta_fallbacks,
    mark_rosetta_fallbacks_reported,
};
use uuid::Uuid;

use crate::CliResult;

async fn send_entries(
    entries: Vec<RosettaFallbackEntry>,
    mut analytics: AnalyticsReporter,
) -> CliResult<()> {
    analytics
        .get_mut()
        .add("binaries", serde_json::to_string(&entries)?);
    analytics.send_now(true).await?;
    mark_rosetta_fallbacks_reported(&entries)?;
    println!("Sent {} Rosetta fallback entries.", entries.len());
    Ok(())
}

pub(crate) async fn send_sip_report(watch: drain::Watch, machine_id: Uuid) -> CliResult<()> {
    let _report_lock = acquire_rosetta_fallback_report_lock()?;
    let entries = load_rosetta_fallbacks()?;
    if entries.is_empty() {
        println!("There are no unreported Rosetta fallback entries.");
        return Ok(());
    }

    send_entries(
        entries,
        AnalyticsReporter::for_event(ReportTarget::MissingX86Binaries, true, watch, machine_id),
    )
    .await
}

pub(crate) async fn prompt_sip_report(
    progress: &ProgressTracker,
    watch: drain::Watch,
    machine_id: Uuid,
) -> CliResult<()> {
    match load_rosetta_fallbacks() {
        Ok(entries) if entries.is_empty() => return Ok(()),
        Ok(_) => {}
        Err(error) => {
            progress.warning(&format!(
                "Failed to read the Rosetta fallback report: {error}"
            ));
            return Ok(());
        }
    }

    progress.warning(
        "mirrord recorded protected macOS binaries that required Rosetta. Run `mirrord diagnose sip-report` to send the report later.",
    );
    if !io::stdin().is_terminal() {
        return Ok(());
    }

    let send = progress.suspend(|| -> io::Result<bool> {
        eprint!("Send the Rosetta fallback report now? y/n: ");
        io::stderr().flush()?;

        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    });

    // report errors _after_ progress.suspend(), otherwise we get a deadlock
    match send {
        Ok(true) => send_sip_report(watch, machine_id).await?,
        Ok(false) => {}
        Err(error) => progress.warning(&format!("Failed to use the report prompt: {error}")),
    }

    Ok(())
}
