"""A program `mirrord exec` holds for a debugger.

Says it runs, then starts a copy of itself as a child, with the hold and `Ready!` bound to this
process's pid, as a child started before the layer removed the variables would inherit them. The
child says which .NET diagnostic ports it inherited, as a .NET child would pause on.

Then it starts a second child with diagnostic ports of its own, as a launch profile sets them, so
the environment it hands that child no longer carries the inherited port.
"""

import os
import subprocess
import sys

if sys.argv[1:] == ["child"]:
    print("its child runs", flush=True)
    ports = os.environ.get("DOTNET_DiagnosticPorts")
    print(f"its child's DOTNET_DiagnosticPorts: {ports}", flush=True)
    sys.exit(0)

print("the held program runs", flush=True)
subprocess.run(
    [sys.executable, __file__, "child"],
    env={
        **os.environ,
        "MIRRORD_LAYER_DEBUG_HOLD": str(os.getpid()),
        "MIRRORD_LAYER_READY": f"simple:{os.getpid()}",
    },
    check=True,
)
subprocess.run(
    [sys.executable, __file__, "child"],
    env={**os.environ, "DOTNET_DiagnosticPorts": "theirs,nosuspend"},
    check=True,
)
