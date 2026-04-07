#!/bin/bash

# Used for collecting the dynamically linked dependencies of needed
# executables so they can later be copied to a FROM scratch image.

set -e

# List of binaries
BINARIES="
$(which iptables-nft)
$(which ip6tables-nft)
$(which iptables-legacy)
$(which ip6tables-legacy)
$(which ss)
$(which conntrack)
$(which sleep)
$(which cat)
"

DEST=/deps

mkdir $DEST

for bin in $BINARIES; do
	cp --parents "$bin" "$DEST/"

	ldd "$bin" | awk '/=>/ {print $3} /^\/lib/ {print $1}' | while read -r lib; do
		cp --parents "$lib" "$DEST/"
	done
done


DEB_HOST_MULTIARCH=$(dpkg-architecture -q DEB_HOST_MULTIARCH)

# Copy iptables dynamic modules
cp -r --parents /usr/lib/$DEB_HOST_MULTIARCH/xtables/ "$DEST/"

# Create iptables lock file
mkdir "$DEST/run/"
touch "$DEST/run/xtables.lock"


# Copy mirrord-agent's (remaining) dependencies.
#
# Note that we cannot do this dynamically like the the rest of the
# executables because we do not have the mirrord-agent executable
# available at this stage. Thus, the dependencies must be hardcoded
# here. This isn't *that* big of a deal though since almost everything
# is statically linked in mirrord-agent so the set of dynamically
# linked dependencies is essentailly fixed.

cp --parents \
   /usr/lib/$DEB_HOST_MULTIARCH/libm.so.6 \
   /usr/lib/$DEB_HOST_MULTIARCH/libpthread.so.0 \
   /usr/lib/$DEB_HOST_MULTIARCH/libdl.so.2 \
   "$DEST/"


# Copy the dynamic linker
cp --parents $(ldd /bin/sh | awk '/ld-linux|ld\.so/{print $1}') "$DEST/"
