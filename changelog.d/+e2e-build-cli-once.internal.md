Speed up CI by building the mirrord CLI and layer once and sharing them with the e2e jobs as an artifact, instead of recompiling them (zigbuild + wizard) in every e2e matrix leg.
