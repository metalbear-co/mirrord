Added `mirrord kubeconfig fix`, which finds non-absolute paths in kubeconfig `exec` fields and interactively replaces them with absolute paths to make them `$PATH`-indepdendent.
