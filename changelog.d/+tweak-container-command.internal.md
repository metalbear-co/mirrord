Update the location of `--` in container command
```
mirrord container [options] <docker/podman/nerdctl> run -- ...
```
Now will be
```
mirrord container [options] -- <docker/podman/nerdctl> run ...
```
or simply
```
mirrord container [options] <docker/podman/nerdctl> run ...
```
