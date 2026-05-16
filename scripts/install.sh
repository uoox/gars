#!/usr/bin/env sh
# gars one-line installer.
#
#   curl -fsSL https://raw.githubusercontent.com/uoox/gars/main/scripts/install.sh | sh
#
# What this does:
#   1. Detect platform.
#   2. Download `gars` + `garstool` (and `SHA256SUMS`) from the matching
#      GitHub release into a temp dir.
#   3. Verify SHA256 (set `GARS_NO_VERIFY=1` to skip).
#   4. Install to `${GARS_PREFIX:-/usr/local}/bin/` (sudo only if needed).
#   5. If running on an interactive terminal, hand off to `garstool` so the
#      user can configure + install service immediately.
#
# Env overrides:
#   GARS_REPO=uoox/gars          which GitHub repo
#   GARS_VERSION=latest|vX.Y.Z   release to install
#   GARS_PREFIX=/usr/local       install prefix (binaries go to PREFIX/bin)
#   GARS_HOME=$HOME/.gars        data directory (printed in summary)
#   GARS_NO_VERIFY=1             skip SHA256 verification (NOT recommended)
#   GARS_NO_GARSTOOL=1           skip the auto-exec garstool at the end
#
# Supported platforms (v0.8+):
#   - macOS aarch64 (Apple Silicon)
#   - Linux x86_64 / aarch64 / armv7
# Windows is not supported (v0.8+).

set -eu

REPO="${GARS_REPO:-uoox/gars}"
VERSION="${GARS_VERSION:-latest}"
PREFIX="${GARS_PREFIX:-/usr/local}"
BIN_DIR="$PREFIX/bin"
GARS_HOME="${GARS_HOME:-"$HOME/.gars"}"

step() { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
info() { printf '   %s\n' "$*"; }
ok()   { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }
err()  { printf '\033[1;31m  ✗\033[0m %s\n' "$*" >&2; }

# ─── 1. Detect platform ────────────────────────────────────────────
step "1/5 探测平台"
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"

case "$os" in
  darwin) os="darwin" ;;
  linux) os="linux" ;;
  *)
    err "不支持的 OS: $os (v0.8+ 只支持 darwin / linux)"
    exit 1
    ;;
esac

case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  armv7l|armv7) arch="armv7" ;;
  *)
    err "不支持的 arch: $arch"
    exit 1
    ;;
esac

# macOS Intel is no longer built (v0.4+).
if [ "$os" = "darwin" ] && [ "$arch" = "x86_64" ]; then
  cat >&2 <<EOF
gars >= v0.4 只编译 Apple Silicon (aarch64) macOS。
要装 Intel Mac 版的话：
  GARS_VERSION=v0.3.0 sh -c "\$(curl -fsSL https://raw.githubusercontent.com/$REPO/main/scripts/install.sh)"
或从源码编译: cargo install --git https://github.com/$REPO --bin gars --bin garstool
EOF
  exit 1
fi
ok "平台: $os-$arch"

if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi
info "release base: $base"

# ─── 2. Choose downloader ─────────────────────────────────────────
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fL --proto '=https' --tlsv1.2 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -O "$2" "$1"; }
else
  err "需要 curl 或 wget"
  exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

asset_gars="gars-$os-$arch"
asset_tool="garstool-$os-$arch"

# ─── 3. Download ──────────────────────────────────────────────────
step "2/5 下载二进制"
info "→ $base/$asset_gars"
fetch "$base/$asset_gars" "$tmp/$asset_gars"
ok "下载 $asset_gars ($(wc -c < "$tmp/$asset_gars") bytes)"

info "→ $base/$asset_tool"
fetch "$base/$asset_tool" "$tmp/$asset_tool"
ok "下载 $asset_tool ($(wc -c < "$tmp/$asset_tool") bytes)"

# ─── 4. Verify ────────────────────────────────────────────────────
if [ "${GARS_NO_VERIFY:-0}" != "1" ]; then
  step "3/5 校验 SHA256"
  info "→ $base/SHA256SUMS"
  fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"
  (
    cd "$tmp"
    grep -E "($asset_gars|$asset_tool)\$" SHA256SUMS > checksums.subset
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c checksums.subset
    elif command -v shasum >/dev/null 2>&1; then
      shasum -a 256 -c checksums.subset
    else
      err "找不到 sha256sum / shasum；用 GARS_NO_VERIFY=1 跳过校验"
      exit 1
    fi
  )
  ok "SHA256 校验通过"
else
  step "3/5 校验 SHA256 (跳过)"
  info "GARS_NO_VERIFY=1，跳过"
fi

# ─── 5. Install ───────────────────────────────────────────────────
step "4/5 安装到 $BIN_DIR"
chmod +x "$tmp/$asset_gars" "$tmp/$asset_tool"
mkdir -p "$tmp/staged"
mv "$tmp/$asset_gars" "$tmp/staged/gars"
mv "$tmp/$asset_tool" "$tmp/staged/garstool"

if [ -w "$BIN_DIR" ] 2>/dev/null; then
  install -m 755 "$tmp/staged/gars" "$BIN_DIR/gars"
  install -m 755 "$tmp/staged/garstool" "$BIN_DIR/garstool"
elif command -v sudo >/dev/null 2>&1; then
  info "用 sudo 写入 $BIN_DIR"
  sudo mkdir -p "$BIN_DIR"
  sudo install -m 755 "$tmp/staged/gars" "$BIN_DIR/gars"
  sudo install -m 755 "$tmp/staged/garstool" "$BIN_DIR/garstool"
else
  err "无法写入 $BIN_DIR 且无 sudo。设 GARS_PREFIX=\$HOME/.local 或用 root 运行。"
  exit 1
fi
ok "$BIN_DIR/gars"
ok "$BIN_DIR/garstool"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    printf '\n\033[1;33m  ⚠  %s\033[0m\n' "$BIN_DIR 不在 PATH 上"
    info "把这一行加到你的 shell rc：export PATH=\"$BIN_DIR:\$PATH\""
    ;;
esac

# ─── 6. Hand off to garstool ──────────────────────────────────────
step "5/5 启动 garstool 完成配置 + 安装服务"
info "数据目录: $GARS_HOME（首次运行 garstool 时创建）"

if [ "${GARS_NO_GARSTOOL:-0}" = "1" ]; then
  info "GARS_NO_GARSTOOL=1，跳过 garstool 自启"
  echo
  echo "完成。下一步: garstool"
  exit 0
fi

# Detect interactivity. When piped via `curl … | sh` the shell's stdin is
# the pipe — so `[ -t 0 ]` is false. Try /dev/tty as fallback for the
# curl-pipe case.
if [ -t 0 ]; then
  exec "$BIN_DIR/garstool"
elif [ -r /dev/tty ] && [ -w /dev/tty ]; then
  info "(从 curl|sh 启动，把 garstool 接到 /dev/tty)"
  exec "$BIN_DIR/garstool" < /dev/tty > /dev/tty
else
  echo
  echo "完成。下一步: $BIN_DIR/garstool"
fi
