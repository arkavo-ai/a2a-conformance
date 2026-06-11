// Shared identity fixture loading for the arkavo-ext-swift harnesses.
//
// The committed P-256 test keys and `manifest.json` live in
// `adapters/shared-fixtures/identity/` (see its README). The dir is located
// via $A2A_IDENTITY_FIXTURES, then cwd-relative (the runner runs from the
// repo root), then relative to the harness executable
// (adapters/arkavo-ext-swift/.build/release/<bin>).
//
// NOTE: this file is duplicated between ServerHarness and ClientHarness
// (executable targets cannot share sources without a library target).

import Crypto
import Foundation

enum IdentityFixtureError: Error, CustomStringConvertible {
    case notFound(String)
    case malformedManifest(String)

    var description: String {
        switch self {
        case .notFound(let detail): return "identity fixtures not found: \(detail)"
        case .malformedManifest(let detail): return "manifest.json: \(detail)"
        }
    }
}

struct IdentityFixtures: Sendable {
    /// The aia-identity-v1 extension URI (spec §1).
    static let extensionURI = "https://arkavo.social/ext/a2a/aia-identity/v1"

    /// Spec-pinned issuer string (`iss`).
    let iss: String
    let clientDid: String
    let serverDid: String
    /// AIA issuer signing key (harness-local mint path; trust root for peers).
    let issuerPrivateKey: P256.Signing.PrivateKey
    /// AIA issuer public key (the verifier trust root).
    let issuerPublicKey: P256.Signing.PublicKey
    /// Target agent key (card signing).
    let serverPrivateKey: P256.Signing.PrivateKey
    /// Presenting agent public key (the `cnf` confirmation key).
    let clientPublicKey: P256.Signing.PublicKey

    static func locate() throws -> URL {
        if let env = ProcessInfo.processInfo.environment["A2A_IDENTITY_FIXTURES"] {
            return URL(fileURLWithPath: env, isDirectory: true)
        }
        let cwdRelative = URL(fileURLWithPath: "adapters/shared-fixtures/identity", isDirectory: true)
        if FileManager.default.fileExists(atPath: cwdRelative.path) {
            return cwdRelative
        }
        // <repo>/adapters/arkavo-ext-swift/.build/release/<bin> -> <repo>/adapters
        let exeRelative = URL(fileURLWithPath: CommandLine.arguments[0])
            .deletingLastPathComponent()
            .appendingPathComponent("../../../shared-fixtures/identity")
            .standardizedFileURL
        if FileManager.default.fileExists(atPath: exeRelative.path) {
            return exeRelative
        }
        throw IdentityFixtureError.notFound(
            "set A2A_IDENTITY_FIXTURES or run from the repo root "
                + "(tried \(cwdRelative.path) and \(exeRelative.path))")
    }

    static func load() throws -> IdentityFixtures {
        let dir = try locate()
        func pem(_ name: String) throws -> String {
            try String(contentsOf: dir.appendingPathComponent(name), encoding: .utf8)
        }
        let manifestData = try Data(contentsOf: dir.appendingPathComponent("manifest.json"))
        guard let manifest = try JSONSerialization.jsonObject(with: manifestData) as? [String: Any]
        else {
            throw IdentityFixtureError.malformedManifest("not a JSON object")
        }
        func field(_ name: String) throws -> String {
            guard let value = manifest[name] as? String else {
                throw IdentityFixtureError.malformedManifest("missing \(name)")
            }
            return value
        }
        return IdentityFixtures(
            iss: try field("iss"),
            clientDid: try field("clientDid"),
            serverDid: try field("serverDid"),
            issuerPrivateKey: try P256.Signing.PrivateKey(
                pemRepresentation: pem("test-issuer.key.pem")),
            issuerPublicKey: try P256.Signing.PublicKey(
                pemRepresentation: pem("test-issuer.pub.pem")),
            serverPrivateKey: try P256.Signing.PrivateKey(
                pemRepresentation: pem("test-server-agent.key.pem")),
            clientPublicKey: try P256.Signing.PublicKey(
                pemRepresentation: pem("test-client-agent.pub.pem"))
        )
    }
}

// Local base64url helpers (ArkavoA2AIdentity's Data extension is internal).

func base64URLEncode(_ data: Data) -> String {
    data.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
}

func base64URLDecode(_ string: String) -> Data? {
    var base64 = string
        .replacingOccurrences(of: "-", with: "+")
        .replacingOccurrences(of: "_", with: "/")
    let remainder = base64.count % 4
    if remainder == 1 { return nil }
    if remainder > 0 { base64 += String(repeating: "=", count: 4 - remainder) }
    return Data(base64Encoded: base64)
}
