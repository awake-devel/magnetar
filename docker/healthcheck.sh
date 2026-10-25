#!/bin/bash
# Docker HEALTHCHECK for magnetar-server: GET /health on the port of MAGNETAR_BIND and require
# "db":true. Plain bash (/dev/tcp), so the image needs neither curl nor wget.
set -euo pipefail
port=${MAGNETAR_BIND##*:}
exec 3<>"/dev/tcp/127.0.0.1/${port:-8787}"
printf 'GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n' >&3
timeout 5 cat <&3 | grep -q '"db":true'
