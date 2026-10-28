Added sanitization to the `JDK_JAVA_OPTIONS` environment variable by removing `javaagent` options.
This fixes some cases of injected variables pointing to non-existent files and causing errors.
Malformed values (those with unmatched quotes) are left unchanged.
Set `feature.magic.java_agents` to `false` to keep the remote value unchanged.
