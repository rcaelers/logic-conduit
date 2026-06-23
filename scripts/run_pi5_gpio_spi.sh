#!/usr/bin/env bash
# Sync and run the Pi 5 GPIO SPI waveform generator remotely.
#
# Edit these values for your Pi. This script intentionally takes no arguments.
set -euo pipefail

PI_USER="robc"
PI_HOST="vision"
REMOTE_DIR="/home/${PI_USER}/u3pro16-test"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOCAL_GENERATOR="${SCRIPT_DIR}/../examples/pi5_gpio_spi.py"
REMOTE_GENERATOR="${REMOTE_DIR}/pi5_gpio_spi.py"
REMOTE="${PI_USER}@${PI_HOST}"

if [[ ! -f "${LOCAL_GENERATOR}" ]]; then
    echo "GPIO generator not found: ${LOCAL_GENERATOR}" >&2
    exit 1
fi

echo "Creating ${REMOTE_DIR} on ${REMOTE}..."
ssh "${REMOTE}" "mkdir -p '${REMOTE_DIR}'"

echo "Syncing Pi GPIO generator..."
rsync -az "${LOCAL_GENERATOR}" "${REMOTE}:${REMOTE_GENERATOR}"

echo "Starting GPIO SPI test on ${REMOTE}..."
echo "Stop it on the Pi with Ctrl-C."
ssh -t "${REMOTE}" "python3 '${REMOTE_GENERATOR}'"
