//! Module responsible for providing [`initialize_hooks`].

use minhook_detours_rs::guard::DetourGuard;

pub fn initialize_hooks<'a>(guard: &mut DetourGuard<'a>) -> anyhow::Result<()> {
    Ok(())
}