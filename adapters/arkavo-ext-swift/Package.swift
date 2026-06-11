// swift-tools-version: 6.1
import PackageDescription

let package = Package(
    name: "arkavo-ext-swift-conformance",
    platforms: [.macOS(.v14)],
    dependencies: [
        // Same SDK pin as the core adapter (adapters/arkavo-swift): the ext
        // adapter IS that adapter, extended with the arkavo identity layer.
        .package(url: "https://github.com/arkavo-ai/a2a-swift.git", from: "0.1.1"),
        // Hummingbird is allowed in the adapter only; the SDK under test
        // (a2a-swift) stays dependency-free.
        .package(url: "https://github.com/hummingbird-project/hummingbird.git", from: "2.5.0"),
        // Extension layer under test. Path dep on the local checkout only:
        // the arkavo-a2a-swift repo stays private until the §7.5 licensing
        // decision, so this adapter builds only where that checkout exists.
        .package(path: "../../../arkavo-a2a-swift"),
        .package(url: "https://github.com/apple/swift-crypto.git", from: "3.0.0"),
    ],
    targets: [
        .executableTarget(
            name: "ServerHarness",
            dependencies: [
                .product(name: "A2A", package: "a2a-swift"),
                .product(name: "A2AServer", package: "a2a-swift"),
                .product(name: "Hummingbird", package: "hummingbird"),
                .product(name: "ArkavoA2AIdentity", package: "arkavo-a2a-swift"),
                .product(name: "ArkavoA2AWire", package: "arkavo-a2a-swift"),
                .product(name: "Crypto", package: "swift-crypto"),
            ]
        ),
        .executableTarget(
            name: "ClientHarness",
            dependencies: [
                .product(name: "A2A", package: "a2a-swift"),
                .product(name: "A2AClient", package: "a2a-swift"),
                .product(name: "ArkavoA2AIdentity", package: "arkavo-a2a-swift"),
                .product(name: "Crypto", package: "swift-crypto"),
            ]
        ),
    ],
    swiftLanguageModes: [.v6]
)
