Stopped the layer from reporting `EOPNOTSUPP` ("Operation not supported on socket") socket errors as hard layer errors, which flooded logs and could kill processes such as Turbopack workers.
