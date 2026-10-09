On Windows, `%TEMP%` is no longer read locally by default. To keep reading it locally, add `"^{{ get_env(name='TEMP') | path_pattern }}/"` to `feature.fs.local`.
