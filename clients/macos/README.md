# macOS client

Thin native client. **License: Apache-2.0.**

- Window and input: AppKit (Swift)
- Decode: VideoToolbox (H.264)
- Render: Metal
- Shared logic: `skylatch-client-core` through its C ABI

## Build

Planned: a Swift package or Xcode project linking the `skylatch-client-core` static library
(`cargo build -p skylatch-client-core --release`). Not implemented yet.
