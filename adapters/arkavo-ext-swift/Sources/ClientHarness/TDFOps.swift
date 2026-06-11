// arkavo-ext: scenario-keyed TDF parts ops (tdf-parts-v1, TDF-HARNESS.md).
//
// - nanotdf-part-roundtrip: receive, locate the #enc part, decrypt with the
//   TEST dek, assert plaintext == the known value; sibling plaintext decodes.
// - tdf-part-wrong-key-fails-closed: decrypt with the WRONG dek -> GCM auth
//   failure -> surface #error.code = kas-denied, no plaintext, sibling still
//   delivered; the A2A op itself returns result (exchange succeeds).
// - b3-url-artifact-integrity: read the url part, verify /b3/<hex> shape, FETCH
//   the blob, verify BLAKE3 == #enc.b3, decrypt. payload-first holds because the
//   server wrote the blob before serving the message (the fetch succeeds).
//
// The SDK (ArkavoA2ATDF) stays scenario-blind; this harness selects the DEK and
// maps the wrong-key denial to kas-denied per the contract.

import A2A
import A2AClient
import ArkavoA2ATDF
import Foundation

#if canImport(FoundationNetworking)
    import FoundationNetworking
#endif

enum TDFFixtureError: Error, CustomStringConvertible {
    case notFound(String)
    var description: String {
        switch self {
        case .notFound(let detail): return "TDF fixtures not found: \(detail)"
        }
    }
}

struct TDFFixtures: Sendable {
    let testDEK: DEK
    let wrongDEK: DEK

    static let knownPlaintext = "hello tdf"
    static let siblingPlaintext = "sibling plaintext"

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

let tdfFixtures: Result<TDFFixtures, any Error> = Result { try TDFFixtures.load() }

/// The encrypted (#enc) part and the first sibling plaintext from a part list.
private func collectParts(_ parts: [Part]) -> (enc: Part?, sibling: String) {
    var enc: Part?
    var sibling = ""
    for part in parts {
        if part.metadata?[TDFExtension.encKey] != nil {
            enc = part
        } else if case .text(let t) = part.content, sibling.isEmpty {
            sibling = t
        }
    }
    return (enc, sibling)
}

func performTDFOp(_ input: InputLine) async -> JSONValue {
    let fx: TDFFixtures
    switch tdfFixtures {
    case .success(let loaded): fx = loaded
    case .failure(let error):
        return harnessErrorOutcome("tdf fixtures unavailable: \(error)")
    }
    let suffix = String(input.scenario.dropFirst("arkavo/tdf/".count))
    let transport = makeTransport(timeoutMs: input.timeoutMs)

    // Run the op and obtain the SDK-encoded result value + the delivered parts.
    let resultValue: JSONValue
    let parts: [Part]
    do {
        switch input.op {
        case "SendMessage":
            let request = try decodeParams(SendMessageRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            let resp = try await client.sendMessage(request)
            resultValue = try encodeToJSONValue(resp)
            switch resp {
            case .task(let task): parts = task.artifacts.flatMap { $0.parts }
            case .message(let message): parts = message.parts
            }
        case "GetTask":
            let request = try decodeParams(GetTaskRequest.self, from: input.params)
            let client = await makeClient(baseUrl: input.baseUrl, transport: transport)
            let task = try await client.getTask(request)
            resultValue = try encodeToJSONValue(task)
            parts = task.artifacts.flatMap { $0.parts }
        default:
            return .object([
                "kind": .string("unsupported"),
                "detail": .string("tdf path does not support op \(input.op)"),
            ])
        }
    } catch {
        return errorOutcome(error)
    }

    let (encOpt, sibling) = collectParts(parts)
    guard let enc = encOpt else {
        return harnessErrorOutcome("no #enc part in the delivered artifact")
    }

    let tdfValue: JSONValue
    switch suffix {
    case "nanotdf-part-roundtrip":
        do {
            let pt = try TDF.decryptInline(part: enc, dek: fx.testDEK)
            tdfValue = .object([
                "scheme": .string("nanotdf"),
                "plaintext": .string(String(decoding: pt, as: UTF8.self)),
                "sibling": .string(sibling),
            ])
        } catch {
            return .object([
                "kind": .string("error"), "errorCode": .null,
                "errorMessage": .string("roundtrip decrypt failed: \(error)"),
            ])
        }
    case "tdf-part-wrong-key-fails-closed":
        do {
            _ = try TDF.decryptInline(part: enc, dek: fx.wrongDEK)
            return .object([
                "kind": .string("error"), "errorCode": .null,
                "errorMessage": .string("fail-closed violated: wrong key yielded plaintext"),
            ])
        } catch {
            // Fail closed: surface #error.code = kas-denied (the harness maps the
            // wrong-key / KAS denial to kas-denied), assert no plaintext present.
            var surfaced = enc
            attachError(to: &surfaced, code: .kasDenied, detail: "unwrap denied")
            let code =
                surfaced.metadata?[TDFExtension.errorKey]?.objectValue?["code"]?.stringValue ?? ""
            tdfValue = .object([
                "errorCode": .string(code),
                "plaintextPresent": .bool(false),
                "sibling": .string(sibling),
            ])
        }
    case "b3-url-artifact-integrity":
        switch await runB3(enc: enc, dek: fx.testDEK, sibling: sibling, transport: transport) {
        case .tdf(let v): tdfValue = v
        case .terminal(let outcome): return outcome
        }
    default:
        return .object([
            "kind": .string("unsupported"),
            "detail": .string("unknown tdf scenario \(suffix)"),
        ])
    }

    // Merge the tdf object into the SDK-encoded result.
    var merged = resultValue.objectValue ?? [:]
    if resultValue.objectValue == nil {
        merged = ["result": resultValue]
    }
    merged["tdf"] = tdfValue
    return resultOutcome(.object(merged))
}

/// runB3 result: either the tdf sub-object to merge, or a terminal outcome.
private enum B3Outcome {
    case tdf(JSONValue)
    case terminal(JSONValue)
}

private func errorOutcomeValue(_ message: String) -> JSONValue {
    let err: [String: JSONValue] = [
        "kind": .string("error"), "errorCode": .null, "errorMessage": .string(message),
    ]
    return .object(err)
}

/// b3-url-artifact-integrity: verify shape, fetch, verify BLAKE3, decrypt.
private func runB3(enc: Part, dek: DEK, sibling: String, transport: URLSessionTransport) async
    -> B3Outcome
{
    guard case .url(let url) = enc.content else {
        return .terminal(errorOutcomeValue("b3 part is not a url part"))
    }
    let encMeta = enc.metadata?[TDFExtension.encKey]?.objectValue
    let b3Expected = encMeta?["b3"]?.stringValue ?? ""

    // Verify the pinned /b3/<hex> shape (64 lowercase hex chars).
    let hexPart = url.components(separatedBy: "/b3/").last ?? ""
    let urlShapeOk =
        hexPart == b3Expected && hexPart.count == 64
        && hexPart.allSatisfy { $0.isHexDigit && !$0.isUppercase }
    guard urlShapeOk, let fetchURL = URL(string: url) else {
        return .terminal(errorOutcomeValue("b3 url shape invalid: \(url)"))
    }

    // FETCH the blob (through the capture proxy).
    let fetched: Data
    do {
        let (data, response) = try await URLSession.shared.data(for: URLRequest(url: fetchURL))
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            return .terminal(
                errorOutcomeValue(
                    "b3 fetch returned \(http.statusCode) (payload-first violated)"))
        }
        fetched = data
    } catch {
        return .terminal(errorOutcomeValue("b3 fetch failed: \(error)"))
    }

    // Verify integrity: BLAKE3(fetched) == #enc.b3 (fail closed on mismatch).
    let b3Verified = (try? verifyB3(fetched: fetched, expectedHex: b3Expected)) ?? false
    if !b3Verified {
        let mismatch: [String: JSONValue] = [
            "urlShapeOk": .bool(true),
            "b3Verified": .bool(false),
            "payloadFirst": .bool(true),
            "errorCode": .string("integrity-failed"),
            "sibling": .string(sibling),
        ]
        return .tdf(.object(mismatch))
    }

    // The blob is the inline NanoTDF archive (manifest+ciphertext); decrypt it.
    guard let inline = try? A2AJSON.decoder().decode(JSONValue.self, from: fetched) else {
        return .terminal(errorOutcomeValue("blob is not inline JSON"))
    }
    let dataPart = Part(
        content: .data(inline),
        metadata: [TDFExtension.encKey: .object(["scheme": .string("nanotdf"), "v": .number(1)])],
        mediaType: TDFExtension.nanotdfMediaType)
    do {
        let pt = try TDF.decryptInline(part: dataPart, dek: dek)
        let ok: [String: JSONValue] = [
            "urlShapeOk": .bool(true),
            "b3Verified": .bool(true),
            "payloadFirst": .bool(true),
            "plaintext": .string(String(decoding: pt, as: UTF8.self)),
            "sibling": .string(sibling),
        ]
        return .tdf(.object(ok))
    } catch {
        return .terminal(errorOutcomeValue("b3 decrypt failed: \(error)"))
    }
}
