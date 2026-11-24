# Windows client

Thin native client. **License: Apache-2.0.**

- Window and input: Win32
- Decode: Media Foundation (H.264)
- Render: Direct3D 11
- Shared logic: `skylatch-client-core` through its C ABI

## Build

Planned: Visual Studio / MSVC with a Rust toolchain; `cargo build -p skylatch-client-core --release`
produces the library the client links. Not implemented yet.
