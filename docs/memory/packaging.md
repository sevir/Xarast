# packaging

Memory note for the **packaging** subsystem (Linux AppImage first; Windows and
macOS in phase 14). Specification in
[`../phases/phase-00-foundations.md`](../phases/phase-00-foundations.md) §W0.4.

## Current state

- `packaging/linux/build-appimage.sh` produces a working AppImage.
- First real build (phase 0): 3.9 MB. **Re-measured 2026-09-23 with the full
  phase 5 stack** (wgpu, egui, winit, AccessKit, zbus portals, clipboard,
  fonts), built exactly as CI does in `ubuntu:22.04`, x86_64: see the size
  table below. It still bundles **no libraries at all**; the binary's only
  `DT_NEEDED` entries are `libgcc_s.so.1`, `libm.so.6`, `libc.so.6` and the
  loader, and its highest glibc symbol is `GLIBC_2.35`.
- `packaging/linux/smoke-test.sh` boots a built image in a bare container
  (no display, no FUSE) and is run by the `distro-smoke` CI job.
- `cargo xtask icons` rasterises `assets/icons/xarast.svg` into the
  freedesktop theme sizes; CI fails if the committed PNGs are stale.
- CI builds x86_64 and aarch64, smoke-tests both, and attaches them to
  releases with zsync metadata.
- `ci.yml` also runs the export regression (XARA-US-0060): jobs `export`
  (ubuntu-24.04 and 22.04; installs `poppler-utils`, `ghostscript`,
  `qpdf`) and `reproducible` (the two machines' export bytes must match);
  the `check` job installs `poppler-utils` so PDF render tests run. The
  manual-only `export-corpus.yml` checks out the fork's corpus directories
  (`CORPUS_TOKEN` secret if private). Details in `export.md`.
- **Performance gates (XARA-US-0061).** `ci.yml` job `perf`
  (ubuntu-24.04, per push) builds `xarast-cli` + `xtask` in release and runs
  `xtask perf --tier pr`; the manual-only `perf-nightly.yml`
  (`workflow_dispatch`, no schedule since 2026-10-22) runs `--tier nightly` with the corpus checked out
  the same way as `export-corpus.yml`. Both upload the results JSON
  (`perf-pr-<sha>`, `perf-nightly-<sha>-<run>`, 90 days) as the per-commit
  history. No apt packages, no GPU, no new Rust dependency (`serde_json`
  was already in the workspace). Gates, margins and noise control are in
  `perf.md`, "CI gates". The AppImage size gate (≤ 80 MiB) stays where it
  was, in `build-appimage.sh`.

## Size (2026-09-23, x86_64, commit 8646a5d)

| Item | Bytes | Note |
|---|---:|---|
| **AppImage** | **8 047 096 (7.7 MiB)** | budget 80 MiB (phase 5); gate now 80 in the script and CI |
| static runtime | 944 632 | `--appimage-offset` |
| squashfs payload | 7 101 282 | zstd, 128 KiB blocks |
| `usr/bin/xarast` in the AppDir | 17 999 033 | stripped by linuxdeploy |
| `target/release/xarast` | 21 867 128 | not stripped (release profile has no `strip`) |
| `xarast-cli` (not in the image) | 2 857 112 | unstripped |

Section sizes of the stripped binary: `.text` 12.9 MB, `.rodata` 2.4 MB,
`.eh_frame` 1.1 MB, `.rela.dyn` 0.6 MB, `.gcc_except_table` 0.5 MB,
`.data.rel.ro` 0.4 MB. `cargo bloat --crates` (host build, same flags) of the
12.3 MiB `.text`: std 2.3 MiB (generic code gets attributed to it), naga 1.3,
zbus 0.9, wgpu_core 0.8, winit 0.67, egui 0.54, wgpu_hal 0.45, zvariant 0.45,
accesskit_unix 0.38, wayland_client 0.27, vello_cpu 0.26; every `xarast_*`
crate together is under 0.7 MiB. The GPU and desktop-integration stack is the
image; our own code is a rounding error. There is ~10x headroom, so no size
work is warranted now; if it ever is, the cheap levers are naga's unused
front-ends (wgpu features) and zbus (via ashpd/accesskit).

## Distro smoke matrix (W0.4.8), 2026-09-23

Run locally with podman on the image above, `APPIMAGE_EXTRACT_AND_RUN=1`, plus
a headless `xarast-cli render` of `Designs/amurdove.xar` (CLI built in the
same container, mounted beside the image).

| Distribution | glibc | --version/--help/--selftest | --selftest-window | loader (NEEDED) | CLI render |
|---|---|---|---|---|---|
| Debian 12 | 2.36 | pass | skipped, exit 0 | 3 libs, none missing | pass, 512x765 |
| Fedora 44 | 2.43 | pass | skipped, exit 0 | 3 libs, none missing | pass |
| openSUSE Tumbleweed | 2.44 | pass | skipped, exit 0 | 3 libs, none missing | pass |
| openSUSE Leap 15.6 | 2.38 | pass | skipped, exit 0 | 3 libs, none missing | pass |
| Arch (SteamOS-like) | 2.44 | pass | skipped, exit 0 | 3 libs, none missing | pass |
| *Ubuntu 20.04 (negative control)* | 2.31 | **fails**: `GLIBC_2.32..2.34 not found` | — | — | — |

Leap 15.6 ships glibc 2.38. Earlier Leap 15.x releases are believed to ship
2.31 (not measured here), which would *not* start the image; the "openSUSE
15.5+" line in phase 0's baseline rationale should be read as "Leap 15.6+".

No compositor or GPU library (`libwayland*`, `libxkbcommon`, `libX11`,
`libxcb`, `libvulkan`, `libGL`, `libEGL`) is linked: all are `dlopen`'d.
On the live host session (Wayland, NVIDIA RTX 4000 Ada, Vulkan) the same image
passed `--selftest-window` (cold start 395 ms) and `--screenshot` of
`amurdove.xar` wrote a 974x1366 PNG and exited on its own.

## Decisions taken (and why)

1. **`linuxdeploy` + `linuxdeploy-plugin-appimage`.** MIT, a single static
   binary, and — crucially — we assemble the AppDir ourselves, so we know
   exactly what ships. `cargo-appimage` gives less control and is
   under-maintained; `appimage-builder` bundles aggressively by default,
   including exactly the libraries that must *not* be bundled.
2. **Bundle nothing that talks to the kernel, the GPU or the compositor.**
   `--exclude-library` covers libGL, libEGL, libgbm, libdrm, libvulkan,
   libwayland-*, libX11/libxcb, libxkbcommon, fontconfig, freetype, libstdc++
   and libgcc_s. A bundled Mesa will not match the host driver and a bundled
   Wayland client will not match the host compositor. This list is the most
   important part of the packaging.
3. **Build inside an explicit `ubuntu:22.04` container, not on the runner.**
   This is not caution, it is a measured requirement: the binary built on this
   development host requires **`GLIBC_2.39`**, which would fail to start on
   Debian 12, Ubuntu 22.04, RHEL 9 or SteamOS. The container pins the baseline
   at glibc **2.35**, and keeps it pinned when GitHub rotates runner images.
4. **Never link musl.** It breaks `dlopen` of the system GL and Vulkan drivers,
   which is the one thing an AppImage must not break.
5. **`APPIMAGE_EXTRACT_AND_RUN=1` everywhere**, local and CI alike, so the two
   paths are the same path. Containers have no FUSE.
6. **`RUSTFLAGS=-C target-cpu=x86-64-v2` on x86_64 only.** SSE4.2 and POPCNT
   have been safe since 2009 and are worth real throughput in the rasteriser.
   aarch64 keeps the default target CPU.
7. **`panic = "unwind"`, not `abort`.** `cargo-fuzz` needs to catch a panic as
   a finding rather than losing the process, and the `.xar` importer is fuzzed
   from day one.
8. **Metadata validators run on `ubuntu-24.04`, not in the build container.**
   `ubuntu:22.04` ships desktop-file-validate 0.26, which rejects the
   entry's `Version=1.5`, and appstreamcli 0.15, which has no `--strict`.
   The `metadata` job runs `desktop-file-validate` and
   `appstreamcli validate --no-net --strict --explain`. `--no-net` so a
   flaky homepage cannot fail a build. The only remaining finding is the
   pedantic hint `cid-contains-uppercase-letter` for `es.digio.Xarast`; it
   is informational and renaming a published component id is not free, so
   it stays.
9. **The distro smoke job is x86_64-only.** `archlinux` has no aarch64
   image and the goal is loader/glibc coverage, which the architecture does
   not change. Each container is capped with `timeout 300`, the job with
   `timeout-minutes: 10`.

## Invariants that must not be broken

1. The Wayland/X11 `app_id`, the desktop entry basename and its
   `StartupWMClass` are all `xarast`. A test in `xarast-shell` enforces it;
   breaking it gives a generic icon and mis-grouped windows, which is invisible
   until someone installs the package.
2. The AppImage must start on glibc 2.35. Verify with `objdump -T` in CI, not
   by hope.
3. No library from the exclusion list may appear in `usr/lib` of the AppDir.
4. Committed icon PNGs must be reproducible from the SVG by `cargo xtask icons`.
5. The binary's `DT_NEEDED` set is libc, libm, libgcc_s and the loader, and
   nothing else. `smoke-test.sh` fails if a compositor or GPU library shows
   up in the loader's list.
6. `--selftest-window` with neither `WAYLAND_DISPLAY` nor `DISPLAY` prints
   `selftest-window: skipped (...)` and exits 0. The smoke job depends on it.

## Dead ends (do not retry)

- Building the release AppImage on the GitHub runner image directly: produces a
  GLIBC_2.39 binary. Measured, not theoretical.
- Relying on FUSE being present anywhere in CI.
- **Running AppImages on a host with AppImageLauncher.** It registers a
  `binfmt_misc` handler (`/proc/sys/fs/binfmt_misc/appimage-type2`, magic
  `AI\x02` at offset 8) whose interpreter is `/usr/bin/AppImageLauncher`.
  `binfmt_misc` is kernel-global, so inside a podman/docker container every
  AppImage (linuxdeploy included) fails with `No such file or directory`,
  exit 127; on the host, running one *moves it into `/apps`*. Work around it
  locally by zeroing bytes 8..10 of a copy
  (`printf '\0\0\0' | dd of=COPY bs=1 seek=8 count=3 conv=notrunc`); the
  type 2 runtime does not check its own magic. GitHub runners have no such
  handler, so CI needs nothing.

## Runtime dependencies the shell introduces (phase 5)

Added by `xarast-shell`, and all of them affect what the AppImage may and
may not bundle. Nothing here changes the exclusion list; it explains why
two more entries belong under it.

| Dependency | Reached how | Packaging consequence |
|---|---|---|
| `libwayland-client` | `winit` with `wayland-dlopen` | **Never bundled**, already excluded. The `dlopen` feature is what lets the binary start on a host with no Wayland library at all, which is why it is not optional. `octotablet 0.1.0` has no equivalent and was rejected partly for that. |
| `libxkbcommon`, `libX11`/`libxcb` | `winit` X11 backend | Already excluded. |
| XDG portals over D-Bus | `ashpd` (FileChooser, Settings) | No library to bundle: it is a D-Bus call through `zbus`, pure Rust. `rfd` is no longer linked on Linux (it spawned `zenity` when the portal failed). A machine with no session bus is handled: every request answers with a reason. |
| Wayland drag and drop | `smithay-client-toolkit` 0.19 / `wayland-client` 0.31 (`client_system`) | The same crates `winit` links; the connection is `winit`'s own `wl_display`, so libwayland-client stays `dlopen`'ed by `winit`'s `wayland-dlopen`. Nothing to bundle. |
| `wl-clipboard-rs` / X11 clipboard | `arboard` with `wayland-data-control` | Pure Rust plus the already-excluded system libraries. |
| Mesa / Vulkan ICDs | `wgpu` | Already excluded, and must stay so. |

The size with all of these linked in is recorded under **Size** above.

## The `.xarast` desktop integration (phase 6 W7, XARA-US-0084)

- `packaging/linux/xarast.xml`: `application/vnd.xarast+zip`, glob
  `*.xarast`, three-level magic (`PK\003\004` at 0, `mimetype` at 30, the
  MIME string at 38), **priority 80** (above generic zip), `sub-class-of
  application/zip`. Checked 2026-09-23 with `update-mime-database` into a
  scratch `XDG_DATA_HOME` and `gio info`: a saved package **with no
  extension** is detected as `application/vnd.xarast+zip`.
- `xarast.desktop` already lists the MIME type (desktop-file-validate
  clean).
- `xarast-thumbnailer` (`xarast-cli`): extracts `thumbnail.png` only,
  checked against the 512 px cap before decoding, box-filtered down to
  `-s SIZE`; never parses `document.svg`, never renders (it runs on every
  file a file manager lists). No thumbnail → exit 1, the file manager
  shows the icon. Registered by `packaging/linux/xarast.thumbnailer`
  (`Exec=xarast-thumbnailer -s %s %i %o`).
- `build-appimage.sh` builds and installs the thumbnailer
  (`usr/bin`, `usr/share/thumbnailers`) and passes it to linuxdeploy as a
  second `--executable`. **Caveat:** files inside an AppImage are not seen
  by the host's MIME database or thumbnail cache until something
  integrates the image (appimaged, Gear Lever, a distro package). Not
  measured yet in a built image.

## Open TODOs

- [ ] Run a built AppImage's `xarast-thumbnailer` on a saved `.xarast`
      in a file manager on a desktop that integrates AppImages.

- [x] Distro smoke matrix (W0.4.8): done locally and wired into CI as
      `distro-smoke` (2026-09-23).
- [x] AppStream validation in CI (`metadata` job).
- [ ] `xarast-cli` is not in the image, so CI's smoke job cannot do a
      headless render (and has no corpus file to render anyway). Tracked as
      XARA-T-0049.
- [ ] GPG signing of the image and the zsync file (phase 12).
- [ ] Phase 12 A5: a dedicated, pinned perf runner (the reference
      machine as a self-hosted runner) so that the real-window cold start
      and the GPU pan budgets can be gated too; today's `perf` jobs run on
      shared GitHub runners, headless and CPU-only (see `perf.md`);
      XARA-T-0298.
- [x] Re-measure the size budget with the phase 5 stack: 7.7 MiB (above).

## Third-party notices and build id (XARA-T-0194, XARA-T-0309)

- `packaging/THIRD-PARTY-NOTICES.txt` is the single source of the
  section 4.1 statements (IJG sentence for `jpeg-encoder`). `xarast-ui`
  embeds it with `include_str!` (`menus::THIRD_PARTY_NOTICES`, shown in a
  scroll area in Help > About) and `build-appimage.sh` installs it as
  `usr/share/doc/xarast/THIRD-PARTY-NOTICES.txt`. A unit test in
  `menus.rs` parses the section 4.1 table of
  `docs/11-licensing-and-clean-room.md` and requires each quoted
  statement in the notices: adding a row there without updating the file
  fails the test. The `appimage.yml` step "Notices and build id" greps
  the AppDir copy. New packages (Windows/macOS) must ship the same file.
- `build-appimage.sh` exports `XARAST_BUILD_ID` (default: 12-char commit
  hash from `git rev-parse`; overridable) before compiling `xarast`.
  `appimage.yml` is also the release workflow (`release: published`), so
  releases are covered. `xarast --version --verbose` prints
  `build: <version> (<id>)`; the same step checks it equals `HEAD`.
- The `reproducible` job builds only `xarast-cli`/`xtask` and never sets
  the id, so its export bytes are unaffected; the id is a commit hash, no
  timestamp, so the same commit still builds the same binary.
