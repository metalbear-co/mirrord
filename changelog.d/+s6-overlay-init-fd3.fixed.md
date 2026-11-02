Fixed `mirrord container` hanging on images that use the s6-overlay `/init` entrypoint, which read the layer's internal proxy connection on fd 3.
