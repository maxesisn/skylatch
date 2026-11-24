# Licensing

Skylatch is a multi-license repository. The boundary is by directory.

| Path | License | Text |
| --- | --- | --- |
| `crates/` (`skylatch-proto`, `skylatch-hub`, `skylatch-client-core`) | Apache-2.0 | [LICENSE-APACHE](LICENSE-APACHE) |
| `clients/` | Apache-2.0 | [LICENSE-APACHE](LICENSE-APACHE) |
| `docs/` | Apache-2.0 | [LICENSE-APACHE](LICENSE-APACHE) |
| `host/` | GPL-3.0 | [LICENSE-GPL-3.0](LICENSE-GPL-3.0) |

Files not listed above (`README.md`, CI configuration, build files) are Apache-2.0.

## Why two licenses

- **Apache-2.0 components** never link `libx264`. They can be embedded in third-party clients,
  built into closed-source products and shipped through app stores (including the Apple App Store)
  without copyleft obligations.
- **`host/`** is derived from the [screen-relay](https://github.com/vercel-labs/screen-relay)
  C++17 capture and encoding engine and links `libx264`, which is GPL-licensed. The host is
  therefore distributed under GPL-3.0. Anyone who redistributes a host binary must offer the
  corresponding source under the GPL.

The host and the other components talk only over the network protocol documented in
[docs/PROTOCOL.md](docs/PROTOCOL.md). The protocol is an interface, not a linkage: a
differently-licensed host or client can implement it independently.

## Attribution

Upstream attribution for `screen-relay` is in [NOTICE](NOTICE).

## Contributions

Contributions to a directory are licensed under that directory's license. Do not move code across
the `host/` boundary in either direction without a licensing review.
