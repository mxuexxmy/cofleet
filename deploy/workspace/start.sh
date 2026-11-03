#!/bin/sh
set -eu
mkdir -p /home/coder/project /home/coder/.local/share/cofleet
if [ -f /home/coder/project/.cofleet/start.sh ]; then
    (cd /home/coder/project && exec /bin/sh .cofleet/start.sh) > /home/coder/.local/share/cofleet/project.log 2>&1 &
fi
exec /usr/bin/code-server --bind-addr 0.0.0.0:8080 --auth none --disable-telemetry --disable-update-check --disable-getting-started-override --enable-remote-storage /home/coder/project
