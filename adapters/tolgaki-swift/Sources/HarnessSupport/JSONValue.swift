// JSONValue.swift
// HarnessSupport
//
// Minimal generic JSON tree used by both harnesses for wire JSON that must
// not be filtered through the SDK's types (scenario files, control API,
// outcome lines). Deliberately SDK-free.

import Foundation

public enum JSONValue: Sendable, Equatable {
    case null
    case bool(Bool)
    case int(Int)
    case double(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    // MARK: - Parsing / serialization

    public static func parse(_ data: Data) throws -> JSONValue {
        let obj = try JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
        return fromAny(obj)
    }

    public static func parse(_ string: String) throws -> JSONValue {
        try parse(Data(string.utf8))
    }

    public static func fromAny(_ any: Any) -> JSONValue {
        switch any {
        case is NSNull:
            return .null
        case let n as NSNumber:
            if CFGetTypeID(n) == CFBooleanGetTypeID() {
                return .bool(n.boolValue)
            }
            // Preserve integers exactly when possible.
            let d = n.doubleValue
            if d == d.rounded(), let i = Int(exactly: n.int64Value), n.doubleValue == Double(i) {
                // Distinguish 2.0 from 2 is impossible via JSONSerialization;
                // emit integral numbers as ints (matches wire fidelity for
                // the conformance corpus, which uses no fractional values).
                return .int(i)
            }
            return .double(d)
        case let s as String:
            return .string(s)
        case let a as [Any]:
            return .array(a.map(fromAny))
        case let o as [String: Any]:
            return .object(o.mapValues(fromAny))
        default:
            return .string(String(describing: any))
        }
    }

    public var anyValue: Any {
        switch self {
        case .null: return NSNull()
        case .bool(let b): return b
        case .int(let i): return i
        case .double(let d): return d
        case .string(let s): return s
        case .array(let a): return a.map(\.anyValue)
        case .object(let o): return o.mapValues(\.anyValue)
        }
    }

    public func serialized(sortedKeys: Bool = true) throws -> Data {
        var options: JSONSerialization.WritingOptions = [.fragmentsAllowed, .withoutEscapingSlashes]
        if sortedKeys { options.insert(.sortedKeys) }
        return try JSONSerialization.data(withJSONObject: anyValue, options: options)
    }

    public func serializedString() throws -> String {
        String(decoding: try serialized(), as: UTF8.self)
    }

    // MARK: - Accessors

    public subscript(key: String) -> JSONValue? {
        if case .object(let o) = self { return o[key] }
        return nil
    }

    public var stringValue: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    public var intValue: Int? {
        switch self {
        case .int(let i): return i
        case .double(let d) where d == d.rounded(): return Int(d)
        default: return nil
        }
    }

    public var boolValue: Bool? {
        if case .bool(let b) = self { return b }
        return nil
    }

    public var arrayValue: [JSONValue]? {
        if case .array(let a) = self { return a }
        return nil
    }

    public var objectValue: [String: JSONValue]? {
        if case .object(let o) = self { return o }
        return nil
    }

    public var isNull: Bool {
        if case .null = self { return true }
        return false
    }
}

// MARK: - stderr logging / stdout discipline

public enum HarnessIO {
    /// All diagnostics go to stderr; stdout carries only protocol lines.
    public static func log(_ message: String) {
        FileHandle.standardError.write(Data(("[harness] " + message + "\n").utf8))
    }

    /// Emit exactly one protocol line on stdout and flush.
    public static func emitLine(_ line: String) {
        FileHandle.standardOutput.write(Data((line + "\n").utf8))
    }
}
