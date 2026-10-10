import Foundation
import SQLite3

public struct DatabaseError: Error, CustomStringConvertible {
    public var message: String
    public var description: String { message }
}

/// A small wrapper over the system SQLite, enough for the phone's store.
final class Database {
    enum Value: Equatable {
        case text(String)
        case int(Int64)
        case blob(Data)
        case null

        var data: Data? {
            if case .blob(let value) = self { return value }
            return nil
        }

        var string: String? {
            if case .text(let value) = self { return value }
            return nil
        }

        var int: Int64? {
            if case .int(let value) = self { return value }
            return nil
        }
    }

    private var handle: OpaquePointer?
    private static let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

    init(path: String) throws {
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX
        guard sqlite3_open_v2(path, &handle, flags, nil) == SQLITE_OK else {
            let message = handle.map { String(cString: sqlite3_errmsg($0)) } ?? "cannot open"
            sqlite3_close(handle)
            throw DatabaseError(message: message)
        }
        // The app, the share sheet and the widgets all open this file.
        sqlite3_busy_timeout(handle, 5000)
        try execute("PRAGMA journal_mode = WAL")
    }

    deinit {
        sqlite3_close(handle)
    }

    func execute(_ sql: String, _ bindings: [Value] = []) throws {
        _ = try query(sql, bindings)
    }

    func query(_ sql: String, _ bindings: [Value] = []) throws -> [[Value]] {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(handle, sql, -1, &statement, nil) == SQLITE_OK else {
            throw DatabaseError(message: String(cString: sqlite3_errmsg(handle)))
        }
        defer { sqlite3_finalize(statement) }
        for (index, value) in bindings.enumerated() {
            let position = Int32(index + 1)
            switch value {
            case .text(let text): sqlite3_bind_text(statement, position, text, -1, Self.transient)
            case .int(let number): sqlite3_bind_int64(statement, position, number)
            case .blob(let data):
                data.withUnsafeBytes { bytes in
                    _ = sqlite3_bind_blob(statement, position, bytes.baseAddress, Int32(data.count), Self.transient)
                }
            case .null: sqlite3_bind_null(statement, position)
            }
        }
        var rows: [[Value]] = []
        while true {
            let result = sqlite3_step(statement)
            if result == SQLITE_DONE { break }
            guard result == SQLITE_ROW else {
                throw DatabaseError(message: String(cString: sqlite3_errmsg(handle)))
            }
            var row: [Value] = []
            for column in 0..<sqlite3_column_count(statement) {
                switch sqlite3_column_type(statement, column) {
                case SQLITE_INTEGER: row.append(.int(sqlite3_column_int64(statement, column)))
                case SQLITE_NULL: row.append(.null)
                case SQLITE_BLOB:
                    let count = Int(sqlite3_column_bytes(statement, column))
                    if let bytes = sqlite3_column_blob(statement, column), count > 0 {
                        row.append(.blob(Data(bytes: bytes, count: count)))
                    } else {
                        row.append(.blob(Data()))
                    }
                default: row.append(.text(sqlite3_column_text(statement, column).map { String(cString: $0) } ?? ""))
                }
            }
            rows.append(row)
        }
        return rows
    }

    var lastInsertedRow: Int64 {
        sqlite3_last_insert_rowid(handle)
    }
}
