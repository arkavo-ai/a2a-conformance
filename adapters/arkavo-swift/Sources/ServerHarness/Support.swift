// Small global helpers for the server harness. These live outside main.swift
// so they are true global functions (Sendable) rather than top-level-code
// locals, which cannot be captured by @Sendable route closures.

import Foundation
import Hummingbird
import NIOCore

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("server-harness: \(message)\n".utf8))
    exit(2)
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
