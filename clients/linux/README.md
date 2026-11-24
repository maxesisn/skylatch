# Linux client

Thin native client. **License: Apache-2.0.**

- Window and input: GTK4
- Decode: VAAPI, with an FFmpeg software fallback
- Shared logic: `skylatch-client-core` through its C ABI

## Build

Planned: Meson or CMake with GTK4 and FFmpeg development packages, linking the
`skylatch-client-core` library (`cargo build -p skylatch-client-core --release`). Not implemented yet.
