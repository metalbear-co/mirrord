FROM debian AS runtime
ARG TARGETARCH

RUN mkdir -p /opt/mirrord/bin && mkdir -p /opt/mirrord/lib && mkdir -p /opt/mirrord/tls
COPY .docker/cli-image/mirrord /opt/mirrord/bin/mirrord
COPY .docker/cli-image/libmirrord_layer.so /opt/mirrord/lib/libmirrord_layer.so
COPY .docker/cli-image/libmirrord_layer_x86_64.so /tmp/libmirrord_layer_x86_64.so

# Create architecture-specific directories and copy libraries
RUN if [ "$TARGETARCH" = "arm64" ]; then \
    mkdir -p /opt/mirrord/lib/aarch64 /opt/mirrord/lib/x86_64 && \
    cp /opt/mirrord/lib/libmirrord_layer.so /opt/mirrord/lib/aarch64/libmirrord_layer.so; \
    elif [ "$TARGETARCH" = "amd64" ]; then \
    mkdir -p /opt/mirrord/lib/x86_64 && \
    cp /opt/mirrord/lib/libmirrord_layer.so /opt/mirrord/lib/x86_64/libmirrord_layer.so; \
    fi

RUN if [ "$TARGETARCH" = "arm64" ] && [ -f /tmp/libmirrord_layer_x86_64.so ]; then \
    cp /tmp/libmirrord_layer_x86_64.so /opt/mirrord/lib/x86_64/libmirrord_layer.so; \
    fi && \
    chmod u+x /opt/mirrord/bin/mirrord && \
    rm -f /tmp/libmirrord_layer_x86_64.so

VOLUME /opt/mirrord

RUN ln -s /opt/mirrord/bin/mirrord /usr/bin/mirrord
