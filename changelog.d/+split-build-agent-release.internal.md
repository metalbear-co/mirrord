Change release.yaml so pushing final tags will occur only on real releases
while manual releases will push into `ghcr.io/metalbear-co/mirrord-staging: ${{ github.sha }}`
so we can leverage github CI for testing images.