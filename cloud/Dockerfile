# Isolated Linux worker image for the MCAO cloud runner (DPES-811).
#
# Pins: the base is Alpine 3.24.2 by index digest (resolved from docker.io/library/alpine:3.24.2),
# pi is @earendil-works/pi-coding-agent@1.3.0 from the lockfile in cloud/config/pi, and Node must be
# >= 22.19. The full pin set lives in cloud/toolchain.json, and cloud/tests/test_worker_image.py
# checks that this file agrees with it.
#
# At container start the only fetch is one HTTPS GET of this attempt's intake object, through the
# short-lived read-only signed URL the launcher passes as an override (mao_cloud.worker_entry). No
# other network use happens at start. Build-time network use is limited to the apk and npm installs
# below. The image holds no credentials, dotfiles, or GUI sockets. Model access happens through the
# worker service account's metadata-server token, never through secrets in the image.
FROM alpine:3.24.2@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6

# PATH keeps Alpine's default directories after the pi bin dir. apk is installed in /sbin, so
# dropping the sbin directories makes every RUN fail with "apk: not found".
ENV PI_OFFLINE=1 \
    PI_SKIP_VERSION_CHECK=1 \
    PI_TELEMETRY=0 \
    LANG=C.UTF-8 \
    HOME=/home/worker \
    PATH=/opt/pi/node_modules/.bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONPATH=/opt/mao-cloud/src

RUN apk upgrade --no-cache

# The manifest and lockfile pin pi and every transitive dependency. The overrides entry in the manifest
# forces undici to 8.12.2 for CVE-2026-109869; pi pins undici 8.10.2 itself, so a bare install would
# keep the vulnerable copy.
COPY --chown=root:root --chmod=0444 cloud/config/pi/package.json cloud/config/pi/package-lock.json cloud/config/pi/check-undici.js /opt/pi/

# --ignore-scripts skips dependency lifecycle scripts, as the proven reference image does. The
# `hash -r` before the npm guard clears the shell's command cache: npm ran earlier in this RUN, and
# without the reset `command -v npm` still reports the binary that `apk del npm` removed.
RUN apk add --no-cache bash bubblewrap ca-certificates fd git jq nodejs npm py3-pydantic python3 ripgrep \
 && node -e 'const [major, minor] = process.versions.node.split(".").map(Number); if (major < 22 || (major === 22 && minor < 19)) { console.error("node " + process.versions.node + " is below 22.19"); process.exit(1); }' \
 && python3 -c 'import sys, pydantic; assert sys.version_info >= (3, 12), sys.version' \
 && npm ci --prefix /opt/pi --ignore-scripts --no-audit --no-fund \
 && node -e 'const pkg = require("/opt/pi/node_modules/@earendil-works/pi-coding-agent/package.json"); if (pkg.version !== "1.3.0") { console.error("pi " + pkg.version); process.exit(1); }' \
 && node /opt/pi/check-undici.js /opt/pi 8.12.2 \
 && apk del npm \
 && rm -rf /root/.npm /var/cache/apk/* /tmp/* \
 && hash -r \
 && if command -v npm >/dev/null 2>&1; then echo "npm is still present" >&2; exit 1; fi

RUN addgroup -g 10001 -S worker \
 && adduser -S -D -H -h /home/worker -u 10001 -G worker worker \
 && mkdir -p /home/worker /work /var/mao/intake \
 && chown worker:worker /home/worker /work /var/mao/intake \
 && chmod 0700 /var/mao/intake

# Sources are relative to the repository root build context (see Dockerfile.dockerignore).
COPY --chown=root:root --chmod=0555 cloud/src/mao_cloud /opt/mao-cloud/src/mao_cloud
COPY --chown=root:root --chmod=0444 cloud/toolchain.json /opt/mao-cloud/toolchain.json

USER 10001:10001
WORKDIR /home/worker

ENTRYPOINT ["python3", "-m", "mao_cloud.worker_entry"]
