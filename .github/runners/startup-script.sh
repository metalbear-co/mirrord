#!/bin/bash
# Startup script executed on runner VM creation.
# Installs base dependencies and mounts the attached cache disk.

set -euxo pipefail

# Install Docker (official convenience script)
curl -fsSL https://get.docker.com -o /tmp/get-docker.sh
sh /tmp/get-docker.sh
rm /tmp/get-docker.sh

# Install required packages
apt-get update
apt-get install -y curl wget git build-essential

# Prepare cache mount point
mkdir -p /mnt/cache
chmod 777 /mnt/cache

# Identify the attached data disk (assumes boot disk is /dev/sda)
DATA_DISK=$(lsblk -ndo NAME,TYPE | awk '$2 == "disk" && $1 != "sda" {print $1; exit}')

if [ -n "${DATA_DISK}" ]; then
  DEVICE_PATH="/dev/${DATA_DISK}"

  # Format disk if it doesn't contain a filesystem
  if ! blkid "${DEVICE_PATH}" >/dev/null 2>&1; then
    mkfs.ext4 -F "${DEVICE_PATH}"
  fi

  # Mount if not already mounted
  if ! mountpoint -q /mnt/cache; then
    mount -o defaults "${DEVICE_PATH}" /mnt/cache
    echo "${DEVICE_PATH} /mnt/cache ext4 defaults 0 2" >> /etc/fstab
  fi

  # Create cache sub-directories
  mkdir -p /mnt/cache/rust
  mkdir -p /mnt/cache/docker
  mkdir -p /mnt/cache/cargo-registry
  chmod -R 777 /mnt/cache
fi






