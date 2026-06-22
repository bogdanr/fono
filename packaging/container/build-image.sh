#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
container_dir="$repo_root/packaging/container"
runtime_packages="busybox-static ca-certificates libgcc-s1 libvulkan1 mesa-vulkan-drivers"

: "${FONO_BINARY:=$repo_root/target/release-slim/fono}"
: "${FONO_IMAGE:=fono:vulkan}"
: "${FONO_BUILD_COMMAND:=cargo build --profile release-slim -p fono --locked --features accel-vulkan}"
: "${FONO_DOCKER_BUILD_OPTS:=}"

if [ ! -x "$FONO_BINARY" ]; then
    printf '%s\n' "Vulkan-capable Fono binary not found or not executable: $FONO_BINARY" >&2
    printf '%s\n' "Build it first with: $FONO_BUILD_COMMAND" >&2
    exit 1
fi

if ! ldd "$FONO_BINARY" 2>/dev/null | grep -q 'libvulkan[.]so[.]1'; then
    printf '%s\n' "Fono binary is not Vulkan-capable: $FONO_BINARY" >&2
    printf '%s\n' "Build it first with: $FONO_BUILD_COMMAND" >&2
    exit 1
fi

context_dir=$(mktemp -d)
cleanup() {
    rm -rf "$context_dir"
}
trap cleanup EXIT INT TERM

cp "$container_dir/Dockerfile" "$context_dir/Dockerfile"
cp "$container_dir/entrypoint.sh" "$context_dir/entrypoint.sh"
cp "$FONO_BINARY" "$context_dir/fono"

build_with_legacy_seccomp_fallback() {
    deps_container="fono-container-deps-$$"
    deps_image="fono-container-deps:$$"

    cleanup_deps() {
        docker rm -f "$deps_container" >/dev/null 2>&1 || true
        docker image rm "$deps_image" >/dev/null 2>&1 || true
    }
    trap 'cleanup_deps; cleanup' EXIT INT TERM

    printf '%s\n' "Building temporary Vulkan runtime base with unconfined seccomp"
    docker run -d --name "$deps_container" --security-opt seccomp=unconfined debian:trixie-slim sleep infinity >/dev/null
    docker exec "$deps_container" sh -c "set -eu; rm -f /etc/apt/apt.conf.d/docker-clean; apt-get update; DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends $runtime_packages; rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/*.deb /var/cache/apt/*.bin; mkdir -p /fono-root/bin /fono-root/etc/ssl /fono-root/usr/lib64 /fono-root/usr/share /data; cp /bin/busybox /fono-root/bin/busybox; cp -a /etc/ssl/certs /fono-root/etc/ssl/certs; cp -a /usr/lib /fono-root/usr/lib; if [ -d /usr/lib64 ]; then cp -a /usr/lib64/. /fono-root/usr/lib64/; fi; cp -a /usr/share/vulkan /fono-root/usr/share/vulkan"
    docker commit "$deps_container" "$deps_image" >/dev/null

    cat > "$context_dir/Dockerfile.fallback" <<EOF_DOCKERFILE
FROM $deps_image AS vulkan-runtime

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
EOF_DOCKERFILE

    docker build -f "$context_dir/Dockerfile.fallback" -t "$FONO_IMAGE" "$context_dir"
}

printf '%s\n' "Building $FONO_IMAGE from isolated context: $context_dir"
if [ "${FONO_LEGACY_SECCOMP_FALLBACK:-auto}" = "always" ]; then
    build_with_legacy_seccomp_fallback
    exit 0
fi

# shellcheck disable=SC2086 # Intentionally allow callers to pass multiple Docker build options.
if ! docker build $FONO_DOCKER_BUILD_OPTS -t "$FONO_IMAGE" "$context_dir"; then
    if [ "${FONO_LEGACY_SECCOMP_FALLBACK:-auto}" = "never" ]; then
        exit 1
    fi
    printf '%s\n' "Docker build failed; retrying with legacy seccomp fallback"
    build_with_legacy_seccomp_fallback
fi
