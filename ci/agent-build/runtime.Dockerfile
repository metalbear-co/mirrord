FROM debian:stable as builder
# We need to build our own conntrack since -U is broken and fixe only in main https://git.netfilter.org/conntrack-tools/commit/?id=a7abf3f5dc7c43f0b25f1d38f754ffc44da54687
RUN apt update && apt install -y gcc bison flex autoconf automake libtool make pkg-config check g++ git libnfnetlink-dev libmnl-dev libnetfilter-conntrack-dev libnetfilter-cttimeout-dev libnetfilter-cthelper-dev libnetfilter-queue-dev libtirpc-dev
WORKDIR /conntrack
RUN git clone git://git.netfilter.org/conntrack-tools
# Current master head
WORKDIR /conntrack/conntrack-tools
RUN git checkout d417ceaa947c5f7f5d691037d0abe1deca957313
RUN ./autogen.sh && ./configure && make -j$(nproc)

RUN cp ./src/conntrack /usr/sbin/conntrack

# iproute2 for using ss to flush
# dpkg-dev for dpkg-architecture, used by collect-deps to find
# the correct arch-specific files
RUN apt update && apt install -y iptables iproute2 dpkg-dev
# RUN update-alternatives --set iptables /usr/sbin/iptables-legacy \
#     && update-alternatives --set ip6tables /usr/sbin/ip6tables-legacy

# Collect dynamically linked dependencies and other executables
COPY agent-build/collect-deps.sh /collect-deps.sh
RUN /collect-deps.sh

FROM scratch

COPY --from=builder /deps/ /
