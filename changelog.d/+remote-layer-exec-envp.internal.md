Keep remote-layer loaded through an `execve` that supplies its own environment.
Inheriting `LD_PRELOAD` covers children that are given the parent's environment,
but a caller building its own `envp` produced a new image with no layer and no
socket hooks.
