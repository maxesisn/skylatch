# macOS packaging

`make-dmg.sh VERSION ARCH BIN_DIR [OUTPUT_DIR]` builds `Skylatch-VERSION-macos-ARCH.dmg`.

## Ad-hoc build (default)
With `CODESIGN_IDENTITY` unset (or `-`) the app is ad-hoc signed. No secrets needed;
this is what CI on public runners uses.

## Developer ID signing
Set `CODESIGN_IDENTITY` to a `Developer ID Application: ...` identity (name or SHA-1)
in the keychain. Bundled dylibs, helper binaries, the app and the DMG are signed with
Hardened Runtime and a secure timestamp. `entitlements.plist` is intentionally empty:
all bundled dylibs are re-signed with the same identity, so library validation stays on.

## Notarization
If `CODESIGN_IDENTITY` is set and all of these are set too, the DMG is submitted with
`xcrun notarytool`, stapled, and checked with `stapler validate` and `spctl`:
- `NOTARY_KEY_PATH`: App Store Connect API key (`.p8`)
- `NOTARY_KEY_ID`: key ID
- `NOTARY_ISSUER_ID`: issuer UUID
