// Small global helpers for the server harness. These live outside main.swift
// so they are true global functions (Sendable) rather than top-level-code
// locals, which cannot be captured by @Sendable route closures.

import ArkavoA2AIdentity
import Foundation
import Hummingbird
import NIOCore

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("server-harness: \(message)\n".utf8))
    exit(2)
}

// MARK: - arkavo-ext: Bearer-CWT enforcement (spec §8)

func unauthorized(_ challenge: String) -> Response {
    Response(
        status: .unauthorized,
        headers: [.wwwAuthenticate: challenge],
        body: ResponseBody(byteBuffer: ByteBuffer()))
}

func invalidToken(_ description: String) -> Response {
    // Quotes stripped so the quoted-string parameter stays well-formed.
    let cleaned = description.replacingOccurrences(of: "\"", with: "'")
    return unauthorized(
        "Bearer realm=\"a2a\", error=\"invalid_token\", error_description=\"\(cleaned)\"")
}

/// Returns a 401 rejection, or nil when the request carries a valid CWT.
/// Missing credential -> `invalid_request`; malformed / invalid / expired /
/// wrong-audience -> `invalid_token` (reconciled pin: RFC 6750, spec §8).
func authenticateBearerCWT(_ request: Request, verifier: CwtVerifier) async -> Response? {
    guard let header = request.headers[.authorization] else {
        return unauthorized("Bearer realm=\"a2a\", error=\"invalid_request\"")
    }
    let trimmed = header.trimmingCharacters(in: .whitespaces)
    guard trimmed.lowercased().hasPrefix("bearer ") else {
        return invalidToken("not a Bearer credential")
    }
    let tokenText = String(trimmed.dropFirst("bearer ".count)).trimmingCharacters(in: .whitespaces)
    guard let token = base64URLDecode(tokenText) else {
        return invalidToken("credential is not base64url")
    }
    do {
        _ = try await verifier.verify(token)
        return nil
    } catch {
        return invalidToken(String(describing: error))
    }
}

func jsonResponse(_ data: Data) -> Response {
    Response(
        status: .ok,
        headers: [.contentType: "application/json"],
        body: ResponseBody(byteBuffer: ByteBuffer(bytes: data)))
}

enum PortEvent: Sendable {
    case a2a(Int)
    case control(Int)
}
