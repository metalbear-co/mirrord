Updated the `experimental` section of the mirrord config:
1. Removed deprecated `readlink` setting.
2. Removed deprecated `readonly_file_buffer` setting, which had been moved to `feature.fs`.
3. Removed `vfork_emulation` setting. vfork emulation is now always enabled.
4. `hook_rename` is now enabled by default.
5. `dns_permission_error_fatal` is now enabled by default.
