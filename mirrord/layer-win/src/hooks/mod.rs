//! Module responsible for providing [`initialize_hooks`].

pub(crate) mod files;
pub(crate) mod macros;
pub(crate) mod process;
pub(crate) mod socket;

use minhook_detours_rs::guard::DetourGuard;

pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> anyhow::Result<()> {
    process::initialize_hooks(guard)?;
    files::initialize_hooks(guard)?;
    socket::initialize_hooks(guard)?;
    guard.enable_all_hooks()?;

    Ok(())
}
