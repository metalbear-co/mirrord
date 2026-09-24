Fixed the WinGet release workflow by passing explicit x64 machine overrides to wingetcreate, because the WiX container is detected as x86 while installed binaries and the published manifest are x64.
