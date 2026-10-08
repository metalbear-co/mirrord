use std::{
    borrow::Cow,
    io::{self, IsTerminal, Write},
};

use mirrord_analytics::{AnalyticsReporter, ReportTarget, Reporter};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_sip::rosetta::{
    RosettaFallbackEntry, acquire_rosetta_fallback_report_lock, load_rosetta_fallbacks,
    mark_rosetta_fallbacks_reported, try_acquire_rosetta_fallback_report_lock,
};
use uuid::Uuid;

use crate::CliResult;

async fn send_entries(
    entries: &[RosettaFallbackEntry],
    mut analytics: AnalyticsReporter,
) -> CliResult<usize> {
    let report_entries: Vec<_> = entries
        .iter()
        .map(|entry| RosettaFallbackEntry {
            binary_path: sanitize_binary_path(&entry.binary_path).into_owned(),
            os_version: entry.os_version.clone(),
        })
        .collect();
    analytics
        .get_mut()
        .add("binaries", serde_json::to_string(&report_entries)?);
    analytics.send_now(true).await?;
    mark_rosetta_fallbacks_reported(entries)?;
    Ok(entries.len())
}

fn sanitize_binary_path(binary_path: &str) -> Cow<'_, str> {
    let Some((prefix, remainder)) = binary_path.split_once("/Users/") else {
        return Cow::Borrowed(binary_path);
    };
    let suffix = remainder
        .find('/')
        .map_or("", |index| remainder.get(index..).unwrap_or_default());
    Cow::Owned(format!("{prefix}/Users/<redacted>{suffix}"))
}

pub(crate) async fn send_sip_report(watch: drain::Watch, machine_id: Uuid) -> CliResult<()> {
    let _report_lock = acquire_rosetta_fallback_report_lock()?;
    let entries = load_rosetta_fallbacks()?;
    if entries.is_empty() {
        println!("There are no unreported Rosetta fallback entries.");
        return Ok(());
    }

    let sent = send_entries(
        &entries,
        AnalyticsReporter::for_event(ReportTarget::MissingX86Binaries, true, watch, machine_id),
    )
    .await?;
    println!("Sent {sent} Rosetta fallback entries.");
    Ok(())
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
    };

    progress.warning(
        "mirrord recorded protected macOS binaries that required Rosetta. Run `mirrord diagnose sip-report` to send the report later.",
    );
    if !io::stdin().is_terminal() {
        return Ok(());
    }

    let _report_lock = match try_acquire_rosetta_fallback_report_lock() {
        Ok(Some(lock)) => lock,
        Ok(None) => return Ok(()),
        Err(error) => {
            progress.warning(&format!(
                "Failed to lock the Rosetta fallback report: {error}"
            ));
            return Ok(());
        }
    };
    let entries = match load_rosetta_fallbacks() {
        Ok(entries) if entries.is_empty() => return Ok(()),
        Ok(entries) => entries,
        Err(error) => {
            progress.warning(&format!(
                "Failed to read the Rosetta fallback report: {error}"
            ));
            return Ok(());
        }
    };

    let send = progress.suspend(|| -> io::Result<bool> {
        eprintln!("The report contains:");
        for entry in &entries {
            eprintln!(
                "  {} (macOS {})",
                sanitize_binary_path(&entry.binary_path),
                entry.os_version,
            );
        }
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
        Ok(true) => {
            let sent = send_entries(
                &entries,
                AnalyticsReporter::for_event(
                    ReportTarget::MissingX86Binaries,
                    true,
                    watch,
                    machine_id,
                ),
            )
            .await?;
            progress.info(&format!("Sent {sent} Rosetta fallback entries."));
        }
        Ok(false) => {}
        Err(error) => progress.warning(&format!("Failed to use the report prompt: {error}")),
    }

    Ok(())
}
