Make Detour helper traits and ergonomics fully cross-platform so they work cleanly on Windows, removing Unix/Windows-specific forks.

Introduce a shared socket helper in layer-lib and unify Unix + Windows socket detours around it as the first real consumer of the cross-platform Detour API.