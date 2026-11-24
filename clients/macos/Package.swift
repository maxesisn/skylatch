// swift-tools-version: 5.9
// SPDX-License-Identifier: Apache-2.0
import Foundation
import PackageDescription

// The Rust core is linked statically so the resulting binary runs without an rpath or a copied dylib.
// Build it first with `cargo build --release -p skylatch-client-core` (clients/macos/build.sh does this).
let packageDir = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
let repoRoot = packageDir.deletingLastPathComponent().deletingLastPathComponent()
let coreLibDir = ProcessInfo.processInfo.environment["SKYLATCH_CORE_LIB_DIR"]
    ?? repoRoot.appendingPathComponent("target/release").path

let package = Package(
    name: "skylatch-macos-client",
    platforms: [.macOS(.v12)],
    products: [
        .executable(name: "skylatch-macos-client", targets: ["skylatch-macos-client"]),
    ],
    targets: [
        // Module map over crates/skylatch-client-core/include/skylatch_client_core.h.
        .systemLibrary(name: "CSkylatchCore", path: "Sources/CSkylatchCore"),
        // Pure logic (Annex B, key map, CLI, input math); no Rust or Apple UI dependencies.
        .target(name: "SkylatchLogic", path: "Sources/SkylatchLogic"),
        .executableTarget(
            name: "skylatch-macos-client",
            dependencies: ["CSkylatchCore", "SkylatchLogic"],
            path: "Sources/skylatch-macos-client",
            linkerSettings: [
                .unsafeFlags(["\(coreLibDir)/libskylatch_client_core.a"]),
                .linkedFramework("AppKit"),
                .linkedFramework("Metal"),
                .linkedFramework("MetalKit"),
                .linkedFramework("VideoToolbox"),
                .linkedFramework("CoreMedia"),
                .linkedFramework("CoreVideo"),
                .linkedFramework("QuartzCore"),
                .linkedFramework("Carbon"),
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("CoreFoundation"),
                .linkedLibrary("iconv"),
                .linkedLibrary("resolv"),
            ]
        ),
        .testTarget(
            name: "SkylatchLogicTests",
            dependencies: ["SkylatchLogic"],
            path: "Tests/SkylatchLogicTests"
        ),
    ]
)
