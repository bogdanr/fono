FROM debian:trixie-slim AS vulkan-runtime

RUN rm -f /etc/apt/apt.conf.d/docker-clean \
    && apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        busybox-static \
        ca-certificates \
        libgcc-s1 \
        libvulkan1 \
        mesa-vulkan-drivers \
    && rm -rf \
        /var/lib/apt/lists/* \
        /var/cache/apt/archives/*.deb \
        /var/cache/apt/*.bin \
    && mkdir -p \
        /fono-root/bin \
        /fono-root/etc/ssl \
        /fono-root/usr/lib64 \
        /fono-root/usr/share \
    && cp /bin/busybox /fono-root/bin/busybox \
    && cp -a /etc/ssl/certs /fono-root/etc/ssl/certs \
    && cp -a /usr/lib /fono-root/usr/lib \
    && if [ -d /usr/lib64 ]; then cp -a /usr/lib64/. /fono-root/usr/lib64/; fi \
    && cp -a /usr/share/vulkan /fono-root/usr/share/vulkan

FROM scratch

LABEL org.opencontainers.image.title="Fono Server"
LABEL org.opencontainers.image.description="Fono server container with Vulkan acceleration and Wyoming protocol support"
LABEL org.opencontainers.image.source="https://github.com/bogdanr/fono"
LABEL org.opencontainers.image.licenses="GPL-3.0-only"

COPY --from=vulkan-runtime /fono-root/ /
COPY fono /usr/local/bin/fono
COPY entrypoint.sh /usr/local/bin/fono-entrypoint

RUN ["/bin/busybox", "--install", "-s", "/bin"]
RUN ["/bin/ln", "-s", "usr/lib", "/lib"]
RUN ["/bin/ln", "-s", "usr/lib64", "/lib64"]
RUN ["/bin/chmod", "0755", "/usr/local/bin/fono", "/usr/local/bin/fono-entrypoint"]

ENV HOME=/data
ENV FONO_LOG=info

WORKDIR /data
VOLUME ["/data"]
EXPOSE 10300/tcp

ENTRYPOINT ["/usr/local/bin/fono-entrypoint"]
CMD ["fono"]
