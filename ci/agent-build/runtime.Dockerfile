FROM debian:stable as builder

RUN apt update && apt install -y gcc bison flex autoconf automake libtool make pkg-config check g++ git libnfnetlink-dev libmnl-dev libnetfilter-conntrack-dev libnetfilter-cttimeout-dev libnetfilter-queue-dev conntrack

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
