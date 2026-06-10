// swift-tools-version: 6.0
// Conformance adapter harnesses for tolgaki's third-party A2A Swift SDKs.
//
// Client SDK: local clone of https://github.com/tolgaki/a2a-swift
//   pinned upstream SHA: 5b0afd9299d2fd6eca2bd93a07140bdd6c0cdabc
// Server SDK: local clone of https://github.com/tolgaki/a2a-swift-server
//   pinned upstream SHA: 0a1db4f759250a23ac6ca9528ca76d5f0ae8ea64
//   (its own dependency graph pulls https://github.com/tolgaki/a2a-client-swift.git
//    from: 1.0.22 as its A2AClient — left untouched.)
//
// NOTE: with path dependencies SPM uses the directory name as the package
// identity, hence package: "tolgaki-a2a-swift" / "tolgaki-a2a-swift-server".
//
// Both SDK graphs vend a target named "A2AClient" (a2a-swift and
// a2a-client-swift), so the client-side products are module-aliased.

import PackageDescription

let package = Package(
    name: "tolgaki-swift-adapter",
    platforms: [.macOS(.v14)],
    dependencies: [
        // For local development flip these to path deps on local clones
        // (package identity then comes from the directory name).
        .package(url: "https://github.com/tolgaki/a2a-swift.git",
                 revision: "5b0afd9299d2fd6eca2bd93a07140bdd6c0cdabc"),
        .package(url: "https://github.com/tolgaki/a2a-swift-server.git",
                 revision: "0a1db4f759250a23ac6ca9528ca76d5f0ae8ea64"),
        // Already in the server SDK's dependency tree; used only for the
        // harness-owned control endpoint (never the A2A endpoint).
        .package(url: "https://github.com/hummingbird-project/hummingbird.git", from: "2.5.0"),
        .package(url: "https://github.com/apple/swift-log.git", from: "1.5.0"),
    ],
    targets: [
        .target(name: "HarnessSupport"),
        .executableTarget(
            name: "ClientHarness",
            dependencies: [
                "HarnessSupport",
                // Only the A2AClient module name collides with the server
                // graph's a2a-client-swift target; A2ACore is unique.
                .product(
                    name: "A2AClient",
                    package: "a2a-swift",
                    moduleAliases: ["A2AClient": "TolgakiA2AClient"]
                ),
            ]
        ),
        .executableTarget(
            name: "ServerHarness",
            dependencies: [
                "HarnessSupport",
                .product(name: "A2AServer", package: "a2a-swift-server"),
                .product(name: "Hummingbird", package: "hummingbird"),
                .product(name: "Logging", package: "swift-log"),
            ]
        ),
    ]
)
