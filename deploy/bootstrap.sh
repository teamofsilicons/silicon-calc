#!/usr/bin/env bash
# Run once as root on the isolated Ubuntu 24.04 ARM64 Calc host.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends build-essential clang pkg-config libssl-dev ca-certificates curl xz-utils unzip python3-venv postgresql postgresql-client apparmor bubblewrap libreoffice-calc-nogui fonts-dejavu-core fonts-liberation caddy
getent passwd calc >/dev/null || useradd --system --home-dir /var/lib/silicon-calc --create-home --shell /usr/sbin/nologin calc
getent passwd calc-web >/dev/null || useradd --system --no-create-home --shell /usr/sbin/nologin calc-web
install -d -o calc -g calc -m 0750 /var/lib/silicon-calc/blobs
install -d -m 0755 /opt/silicon-calc/releases /opt/silicon-calc/build
install -d -m 0700 /etc/silicon-calc
if [[ ! -x /usr/local/bin/aws ]]; then
  curl -fsSL https://awscli.amazonaws.com/awscli-exe-linux-aarch64.zip -o /tmp/calc-awscli.zip
  unzip -q /tmp/calc-awscli.zip -d /tmp/calc-awscli
  /tmp/calc-awscli/aws/install
  rm -rf /tmp/calc-awscli /tmp/calc-awscli.zip
fi
if [[ ! -x /opt/node/bin/node ]]; then
  curl -fsSL https://nodejs.org/dist/v24.21.0/node-v24.21.0-linux-arm64.tar.xz -o /tmp/calc-node.tar.xz
  echo '6ad1325edbdb5649c379b75a237147a666c95d4f9ae8d340fef2d1575d289ad2  /tmp/calc-node.tar.xz' | sha256sum -c -
  install -d /opt/node
  tar -xJf /tmp/calc-node.tar.xz -C /opt/node --strip-components=1
  rm /tmp/calc-node.tar.xz
fi
export PATH=/opt/node/bin:/root/.cargo/bin:$PATH
npm install --global pnpm@10.33.0
if [[ ! -x /root/.cargo/bin/rustup ]]; then
  curl -fsSL https://sh.rustup.rs -o /tmp/calc-rustup.sh
  sh /tmp/calc-rustup.sh -y --profile minimal --default-toolchain 1.98.0
  rm /tmp/calc-rustup.sh
fi
rustup toolchain install 1.98.0 --profile minimal
python3 -m venv /opt/silicon-calc/uv
/opt/silicon-calc/uv/bin/pip install --disable-pip-version-check uv==0.9.28
export UV_PYTHON_INSTALL_DIR=/opt/silicon-calc/python
/opt/silicon-calc/uv/bin/uv python install 3.14
/opt/silicon-calc/uv/bin/uv venv --python 3.14 /opt/silicon-calc/runner-venv
chmod -R a+rX /opt/silicon-calc/python
systemctl enable --now postgresql
if ! runuser -u postgres -- psql -Atc "SELECT 1 FROM pg_roles WHERE rolname='calc'" | grep -qx 1; then
  runuser -u postgres -- createuser calc
fi
if ! runuser -u postgres -- psql -Atc "SELECT 1 FROM pg_database WHERE datname='silicon_calc'" | grep -qx 1; then
  runuser -u postgres -- createdb --owner calc silicon_calc
fi
# Peer auth over the Unix socket; no database password or external database port.
runuser -u calc -- psql -d silicon_calc -Atc 'SELECT current_user'
install -m 0644 "$(dirname "${BASH_SOURCE[0]}")/silicon-calc.apparmor" /etc/apparmor.d/silicon-calc
apparmor_parser -r /etc/apparmor.d/silicon-calc
# SSM's own AppArmor profile grants userns, so a runuser probe is misleading.
# Exercise the same explicit profile and outer restrictions as calc-api.
systemd-run --unit="calc-bootstrap-sandbox-$$" --collect --wait --pipe --uid=calc --gid=calc \
  -p RuntimeMaxSec=20 -p AppArmorProfile=silicon-calc-converters \
  -p NoNewPrivileges=true -p PrivateTmp=true -p ProtectSystem=strict \
  -p ProtectHome=true -p ProtectKernelModules=true -p ProtectControlGroups=true \
  -p RestrictSUIDSGID=true /usr/bin/env -i /usr/bin/bwrap --die-with-parent \
  --unshare-all --ro-bind / / --proc /proc --dev /dev -- /usr/bin/true
node --version
rustc --version
echo 'Calc host bootstrap complete'
