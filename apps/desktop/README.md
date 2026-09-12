# OpenCut Desktop

Built with [GPUI](https://www.gpui.rs).

> [!WARNING]
> Very early. Right now this is just a window that opens.

## Running

Rust is pinned in `.prototools` at the repo root (`proto use` installs it).

```sh
moon run desktop:dev     # cargo run
moon run desktop:check   # cargo check
moon run desktop:build   # cargo build --release
```

The first build compiles GPUI from source and takes a while. The root `Cargo.lock` is committed.

## Platform requirements

- **macOS**: Xcode with the Metal toolchain, not just the Command Line Tools. GPUI's build script compiles `shaders.metal` through `xcrun metal`; if that command is missing, install Xcode and run `xcodebuild -downloadComponent MetalToolchain`.
- **Windows**: Visual Studio Build Tools with the "Desktop development with C++" workload. The `x86_64-pc-windows-msvc` target needs the MSVC linker (`link.exe`); without it `cargo` fails with `linker 'link.exe' not found`. Rendering is Win32 + DirectWrite, no runtime dependencies.
- **Linux**: renders via Vulkan (Blade), windows via Wayland or X11 (both enabled by default). System packages (Debian/Ubuntu names): `libvulkan1` + working Vulkan drivers, `libwayland-dev`, `libx11-xcb-dev`, `libxkbcommon-x11-dev`, `libfontconfig-dev`, plus a C toolchain and `cmake`.
- **WSL2/WSLg**: uses XWayland automatically when available. GPUI 0.2.2 requires `xdg_wm_base` v2–5, while WSLg advertises v1.
