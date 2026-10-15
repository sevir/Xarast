#!/usr/bin/env bash
#
# Builds the Xarast AppImage.
#
# Run it from anywhere; it locates the repository itself. In CI it runs inside
# an ubuntu:22.04 container so that the glibc baseline is 2.35 and reproducible
# when GitHub rotates its runner images — see docs/phases/phase-00-foundations.md.
#
#   ARCH          target architecture (default: the host's)
#   OUTPUT_DIR    where the .AppImage lands (default: <repo>/dist)
#   XARAST_BUILD_ID  build id compiled into xarast (default: short commit hash)
#   SKIP_BUILD   set to 1 to package an already-built binary
#   UPDATE_INFO   zsync update information embedded in the image
#   SIZE_LIMIT_MIB  fail above this many MiB (default: 80)
#
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
arch="${ARCH:-$(uname -m)}"
output_dir="${OUTPUT_DIR:-$repo_root/dist}"
appdir="$repo_root/build/AppDir"
tools_dir="${TOOLS_DIR:-$repo_root/build/tools}"

# The GitHub release the built image will advertise for delta updates. zsync
# lets a user fetch only the changed blocks instead of the whole image.
update_info="${UPDATE_INFO:-gh-releases-zsync|digiogithub|Xarast|latest|Xarast-*-$arch.AppImage.zsync}"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  log "Building the release binary"
  # x86-64-v2 (SSE4.2, POPCNT) has been safe since 2009 and is worth real
  # throughput in the rasteriser. aarch64 keeps the default target CPU.
  if [[ "$arch" == "x86_64" ]]; then
    export RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=x86-64-v2"
  fi
  # The commit names the build in crash reports and in
  # `xarast --version --verbose`. A short hash only: no timestamp, so the
  # same commit always yields the same bytes.
  if [[ -z "${XARAST_BUILD_ID:-}" ]]; then
    XARAST_BUILD_ID="$(git -C "$repo_root" rev-parse --short=12 HEAD 2>/dev/null || true)"
  fi
  export XARAST_BUILD_ID
  cargo build --release --locked -p xarast-shell --bin xarast
  cargo build --release --locked -p xarast-cli --bin xarast-thumbnailer
fi

binary="$repo_root/target/release/xarast"
[[ -x "$binary" ]] || { echo "no binary at $binary" >&2; exit 1; }
thumbnailer="$repo_root/target/release/xarast-thumbnailer"
[[ -x "$thumbnailer" ]] || { echo "no binary at $thumbnailer" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Assemble the AppDir
# ---------------------------------------------------------------------------

log "Assembling the AppDir"
rm -rf "$appdir"
mkdir -p "$appdir/usr/bin" \
         "$appdir/usr/share/applications" \
         "$appdir/usr/share/metainfo" \
         "$appdir/usr/share/doc/xarast" \
         "$appdir/usr/share/mime/packages" \
         "$appdir/usr/share/thumbnailers"

install -m 755 "$binary" "$appdir/usr/bin/xarast"
# File managers run the thumbnailer on every .xarast they list; it only
# extracts the package's own thumbnail.png (phase 6 F7.3).
install -m 755 "$thumbnailer" "$appdir/usr/bin/xarast-thumbnailer"
install -m 644 "$repo_root/packaging/linux/xarast.thumbnailer" \
               "$appdir/usr/share/thumbnailers/xarast.thumbnailer"
install -m 644 "$repo_root/packaging/linux/xarast.desktop" \
               "$appdir/usr/share/applications/xarast.desktop"
install -m 644 "$repo_root/packaging/linux/es.digio.Xarast.metainfo.xml" \
               "$appdir/usr/share/metainfo/es.digio.Xarast.metainfo.xml"
install -m 644 "$repo_root/packaging/linux/xarast.xml" \
               "$appdir/usr/share/mime/packages/xarast.xml"

# The third-party notices the IJG licence (jpeg-encoder) requires a binary
# distribution to carry; the same file the About box embeds.
install -m 644 "$repo_root/packaging/THIRD-PARTY-NOTICES.txt" \
               "$appdir/usr/share/doc/xarast/THIRD-PARTY-NOTICES.txt"

icon_root="$repo_root/assets/icons/hicolor"
[[ -d "$icon_root" ]] || { echo "icons missing; run 'cargo xtask icons'" >&2; exit 1; }
for dir in "$icon_root"/*/apps; do
  size="$(basename "$(dirname "$dir")")"
  target="$appdir/usr/share/icons/hicolor/$size/apps"
  mkdir -p "$target"
  cp "$dir"/xarast.* "$target/"
done
# linuxdeploy also wants the icon and desktop entry at the AppDir root.
cp "$icon_root/256x256/apps/xarast.png" "$appdir/xarast.png"

# ---------------------------------------------------------------------------
# Tools
# ---------------------------------------------------------------------------

mkdir -p "$tools_dir"
fetch_tool() {
  local name="$1" url="$2"
  if [[ ! -x "$tools_dir/$name" ]]; then
    log "Fetching $name"
    curl -sSL -o "$tools_dir/$name" "$url"
    chmod +x "$tools_dir/$name"
  fi
}
base="https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous"
fetch_tool "linuxdeploy" "$base/linuxdeploy-$arch.AppImage"
fetch_tool "linuxdeploy-plugin-appimage" \
  "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-$arch.AppImage"

# Containers rarely have FUSE. Every AppImage tool understands this flag, and
# using it everywhere means the CI path and the local path are the same path.
export APPIMAGE_EXTRACT_AND_RUN=1

# ---------------------------------------------------------------------------
# Bundle
# ---------------------------------------------------------------------------
#
# Anything that talks to the kernel, the GPU or the compositor must come from
# the user's system, not from us: a bundled Mesa or libdrm will not match the
# host driver, and a bundled Wayland client library will not match the host
# compositor. This exclusion list is the single most important part of the
# packaging, and the reason we assemble the AppDir by hand.

excludes=(
  libGL.so.1 libGLX.so.0 libGLdispatch.so.0 libEGL.so.1 libOpenGL.so.0
  libgbm.so.1 libdrm.so.2 libvulkan.so.1
  libwayland-client.so.0 libwayland-cursor.so.0 libwayland-egl.so.1
  libX11.so.6 libX11-xcb.so.1 libxcb.so.1 libxkbcommon.so.0
  libfontconfig.so.1 libfreetype.so.6
  libstdc++.so.6 libgcc_s.so.1
)
exclude_args=()
for lib in "${excludes[@]}"; do exclude_args+=(--exclude-library "$lib"); done

log "Running linuxdeploy"
mkdir -p "$output_dir"
# linuxdeploy-plugin-appimage writes the .zsync file into the working
# directory rather than beside OUTPUT, so run it from there.
pushd "$output_dir" >/dev/null
OUTPUT="$output_dir/Xarast-$arch.AppImage" \
UPDATE_INFORMATION="$update_info" \
LINUXDEPLOY_OUTPUT_VERSION="$(git -C "$repo_root" describe --tags --always --dirty 2>/dev/null || echo 0.0.1)" \
"$tools_dir/linuxdeploy" \
  --appdir "$appdir" \
  --executable "$appdir/usr/bin/xarast" \
  --executable "$appdir/usr/bin/xarast-thumbnailer" \
  --desktop-file "$appdir/usr/share/applications/xarast.desktop" \
  --icon-file "$icon_root/256x256/apps/xarast.png" \
  "${exclude_args[@]}" \
  --output appimage
popd >/dev/null

image="$output_dir/Xarast-$arch.AppImage"
[[ -f "$image" ]] || { echo "linuxdeploy produced no image" >&2; exit 1; }
chmod +x "$image"

size_mib=$(( $(stat -c%s "$image") / 1024 / 1024 ))
log "Built $image (${size_mib} MiB)"

# Size gate. An AppImage that quietly grows is an AppImage nobody downloads.
# 35 MiB was the phase 0 budget; 80 MiB is the phase 5 one, with the whole UI
# stack linked. Measured at 7.7 MiB on 2026-09-23 (docs/memory/packaging.md).
limit="${SIZE_LIMIT_MIB:-80}"
if (( size_mib > limit )); then
  echo "AppImage is ${size_mib} MiB, over the ${limit} MiB budget" >&2
  exit 1
fi
