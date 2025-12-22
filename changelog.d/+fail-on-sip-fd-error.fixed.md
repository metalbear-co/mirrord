mirrord will now fail (panic) if SIP patching encounters the `"Too many open files"` error. To fix this, the user can
try increasing the file limit using `ulimit`.