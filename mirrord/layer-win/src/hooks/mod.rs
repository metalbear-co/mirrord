//! Module responsible for providing [`initialize_hooks`].

mod process;

use minhook_detours_rs::guard::DetourGuard;

pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
    process::initialize_hooks(guard)?;
    guard.enable_all_hooks()?;

    Ok(())
}
