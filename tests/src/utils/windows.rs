use std::process::Command;

/// Temporarily switches the Windows console host to legacy mode (ForceV2=0) and restores the
/// original registry setting when dropped.
pub struct LegacyConsoleGuard {
    previous: Option<u32>,
    changed: bool,
}

pub type LegacyConsoleResult<T> = Result<T, String>;

impl LegacyConsoleGuard {
    /// Enables legacy console mode and returns a guard that restores the previous value on drop.
    pub fn enable() -> LegacyConsoleResult<Self> {
        let previous = query_force_v2()?;
        let mut guard = Self {
            previous,
            changed: false,
        };

        if previous != Some(0) {
            set_force_v2(Some(0))?;
            guard.changed = true;
        }

        Ok(guard)
    }
}

impl Drop for LegacyConsoleGuard {
    fn drop(&mut self) {
        if !self.changed {
            return;
        }

        let restore = if let Some(value) = self.previous {
            set_force_v2(Some(value))
        } else {
            set_force_v2(None)
        };

        if let Err(err) = restore {
            eprintln!("failed to restore console registry state: {err}");
        }
    }
}

fn query_force_v2() -> LegacyConsoleResult<Option<u32>> {
    let output = Command::new("reg")
        .args(["query", "HKCU\\Console", "/v", "ForceV2"])
        .output()
        .map_err(|err| format!("failed to query ForceV2 registry value: {err}"))?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("ForceV2") {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if let Some(value_str) = parts.last() {
            let parsed = if let Some(hex) = value_str.strip_prefix("0x") {
                u32::from_str_radix(hex, 16).map_err(|err| {
                    format!("invalid hex ForceV2 registry value {value_str}: {err}")
                })?
            } else {
                value_str
                    .parse::<u32>()
                    .map_err(|err| format!("invalid ForceV2 registry value {value_str}: {err}"))?
            };
            return Ok(Some(parsed));
        }
    }

    Ok(None)
}

fn set_force_v2(value: Option<u32>) -> LegacyConsoleResult<()> {
    let mut command = Command::new("reg");

    if let Some(v) = value {
        command.args([
            "add",
            "HKCU\\Console",
            "/v",
            "ForceV2",
            "/t",
            "REG_DWORD",
            "/d",
        ]);
        command.arg(v.to_string());
        command.arg("/f");
    } else {
        command.args(["delete", "HKCU\\Console", "/v", "ForceV2", "/f"]);
    }

    let output = command
        .output()
        .map_err(|err| format!("failed to update ForceV2 registry value: {err}"))?;

    if output.status.success() {
        return Ok(());
    }

    if value.is_none() {
        if let Some(code) = output.status.code() {
            if code == 1 || code == 2 {
                // Treat missing value as already clean.
                return Ok(());
            }
        }
    }

    Err(format!(
        "registry update for ForceV2 failed (exit {:?}): {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    ))
}
