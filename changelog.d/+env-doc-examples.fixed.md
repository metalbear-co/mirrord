The `feature` and `feature.env` config examples in the docs no longer set both `include` and `exclude`. mirrord does not allow them together, so `mirrord verify-config` rejected these examples.
