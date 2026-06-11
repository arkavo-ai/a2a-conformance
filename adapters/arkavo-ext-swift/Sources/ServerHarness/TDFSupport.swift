// arkavo-ext: TDF parts (tdf-parts-v1, TDF-HARNESS.md) server-side support.
//
// Scenario-keyed in the harness: while an arkavo/tdf/* scenario is selected the
// scripted response carries a shape-(a) NanoTDF part (or a shape-(b) /b3/<hex>
// url part) plus a sibling plaintext part, and the b3 ciphertext blob is written
// payload-first to an in-memory store served by GET /b3/<hex>. The SDK
// (ArkavoA2ATDF) stays scenario-blind.

import A2A
import ArkavoA2ATDF
import Foundation

// MARK: - Pinned constants

enum TDFConstants {
    /// The pinned roundtrip plaintext (shared fixtures `hello-tdf`).
    static let knownPlaintext = "hello tdf"
    /// The pinned sibling plaintext (proves mixed messages, §5).
    static let siblingPlaintext = "sibling plaintext"
    /// Fixed 12-byte nonce for live runs (peers read it from the manifest).
    static let nonce = Data(repeating: 0xAA, count: 12)
    /// KAS URL pinned into the manifest (spec §1 default).
    static let kasUrl = "https://kas.arkavo.net"
}

// MARK: - Fixtures

enum TDFFixtureError: Error, CustomStringConvertible {
    case notFound(String)
    var description: String {
        switch self {
        case .notFound(let detail): return "TDF fixtures not found: \(detail)"
        }
    }
}

/// The committed test DEKs (test-dek.bin = correct key, wrong-dek.bin = the
/// KAS-denial leg's wrong key). Located like the identity fixtures.
struct TDFFixtures: Sendable {
    let testDEK: DEK
    let wrongDEK: DEK

    static func locate() throws -> URL {
        if let env = ProcessInfo.processInfo.environment["A2A_TDF_FIXTURES"] {
            return URL(fileURLWithPath: env, isDirectory: true)
        }
        let cwdRelative = URL(fileURLWithPath: "adapters/shared-fixtures/tdf", isDirectory: true)
        if FileManager.default.fileExists(atPath: cwdRelative.path) {
            return cwdRelative
        }
        let exeRelative = URL(fileURLWithPath: CommandLine.arguments[0])
            .deletingLastPathComponent()
            .appendingPathComponent("../../../shared-fixtures/tdf")
            .standardizedFileURL
        if FileManager.default.fileExists(atPath: exeRelative.path) {
            return exeRelative
        }
        throw TDFFixtureError.notFound(
            "set A2A_TDF_FIXTURES or run from the repo root (tried \(cwdRelative.path))")
    }

    static func load() throws -> TDFFixtures {
        let dir = try locate()
        let test = try Data(contentsOf: dir.appendingPathComponent("test-dek.bin"))
        let wrong = try Data(contentsOf: dir.appendingPathComponent("wrong-dek.bin"))
        return TDFFixtures(testDEK: try DEK(test), wrongDEK: try DEK(wrong))
    }
}

// MARK: - tdf-parts-v1 §1 advertisement params

func tdfExtensionParams() -> [String: JSONValue] {
    [
        "schemes": .array([.string("nanotdf"), .string("tdf")]),
        "kas": .array([.string("https://kas.arkavo.net")]),
        "gateway": .string("https://tdf.arkavo.net"),
    ]
}

// MARK: - JSON helpers

/// Encode a JSONValue to bytes with sorted keys (so the injected respond decodes
/// deterministically through the SDK).
func encodeJSON(_ value: JSONValue) -> Data {
    (try? A2AJSON.encoder().encode(value)) ?? Data("{}".utf8)
}

/// Decode bytes to a JSONValue (the scripted respond JSON).
func decodeJSON(_ data: Data) -> JSONValue? {
    try? A2AJSON.decoder().decode(JSONValue.self, from: data)
}
