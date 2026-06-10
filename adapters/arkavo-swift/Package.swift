// swift-tools-version: 6.1
import PackageDescription

let package = Package(
    name: "arkavo-swift-conformance",
    platforms: [.macOS(.v14)],
    dependencies: [
        // Local path while developing against the working tree. Same code as
        // the released tag; flip to the URL form for a pinned checkout:
        // For local development flip to: .package(path: "../../../../a2a-swift"),
        .package(url: "https://github.com/arkavo-ai/a2a-swift.git", from: "0.1.0"),
        // Hummingbird is allowed in the adapter only; the SDK under test
        // (a2a-swift) stays dependency-free.
        .package(url: "https://github.com/hummingbird-project/hummingbird.git", from: "2.5.0"),
    ],
    targets: [
        .executableTarget(
            name: "ServerHarness",
            dependencies: [
                .product(name: "A2A", package: "a2a-swift"),
                .product(name: "A2AServer", package: "a2a-swift"),
                .product(name: "Hummingbird", package: "hummingbird"),
            ]
        ),
        .executableTarget(
            name: "ClientHarness",
            dependencies: [
                .product(name: "A2A", package: "a2a-swift"),
                .product(name: "A2AClient", package: "a2a-swift"),
            ]
        ),
    ],
    swiftLanguageModes: [.v6]
)
