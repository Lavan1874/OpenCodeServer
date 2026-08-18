import Darwin
import Foundation

/// Errors returned while reading a private, application-owned state file.
///
/// The path is opened before any metadata is trusted. The descriptor is then
/// checked for type, ownership, permissions, and bounded size before its
/// contents are read. Callers can distinguish a first-run missing file from a
/// file that must not be replaced or interpreted.
enum PrivateStateFileReadError: LocalizedError, Equatable {
    case notFound
    case symbolicLink
    case openFailed(String)
    case statFailed(String)
    case notRegular
    case wrongOwner
    case insecurePermissions
    case tooLarge(limit: Int)
    case readFailed(String)

    var errorDescription: String? {
        switch self {
        case .notFound:
            "The private state file does not exist."
        case .symbolicLink:
            "The private state file must not be a symbolic link."
        case let .openFailed(message):
            "The private state file could not be opened: \(message)."
        case let .statFailed(message):
            "The private state file could not be inspected: \(message)."
        case .notRegular:
            "The private state file must be a regular file."
        case .wrongOwner:
            "The private state file must be owned by the current user."
        case .insecurePermissions:
            "The private state file must not be accessible by group or other users."
        case let .tooLarge(limit):
            "The private state file exceeds the \(limit)-byte limit."
        case let .readFailed(message):
            "The private state file could not be read: \(message)."
        }
    }
}

/// Descriptor-first reader for small, user-owned application state.
///
/// `O_NOFOLLOW` closes the final-component symlink race and `fstat` checks the
/// object that will actually be read. The size check is intentionally both
/// metadata-based and read-based: a file that grows after `fstat` is still
/// rejected without allowing an unbounded allocation.
enum PrivateStateFileReader {
    static func read(at url: URL, maxBytes: Int) throws -> Data {
        precondition(maxBytes >= 0)

        let descriptor: Int32 = url.withUnsafeFileSystemRepresentation { path in
            guard let path else {
                errno = EINVAL
                return -1
            }
            return Darwin.open(
                path,
                O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK
            )
        }
        guard descriptor >= 0 else {
            let errorCode = errno
            if errorCode == ENOENT {
                throw PrivateStateFileReadError.notFound
            }
            if errorCode == ELOOP {
                throw PrivateStateFileReadError.symbolicLink
            }
            throw PrivateStateFileReadError.openFailed(
                String(cString: strerror(errorCode))
            )
        }
        defer { Darwin.close(descriptor) }

        var metadata = stat()
        guard Darwin.fstat(descriptor, &metadata) == 0 else {
            throw PrivateStateFileReadError.statFailed(
                String(cString: strerror(errno))
            )
        }
        guard (metadata.st_mode & S_IFMT) == S_IFREG else {
            throw PrivateStateFileReadError.notRegular
        }
        guard metadata.st_uid == geteuid() else {
            throw PrivateStateFileReadError.wrongOwner
        }
        guard (metadata.st_mode & 0o077) == 0 else {
            throw PrivateStateFileReadError.insecurePermissions
        }
        guard metadata.st_size >= 0,
              UInt64(metadata.st_size) <= UInt64(maxBytes)
        else {
            throw PrivateStateFileReadError.tooLarge(limit: maxBytes)
        }

        let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: false)
        var data = Data()
        data.reserveCapacity(min(Int(metadata.st_size), maxBytes))
        do {
            while true {
                let remaining = maxBytes - data.count
                guard remaining >= 0 else {
                    throw PrivateStateFileReadError.tooLarge(limit: maxBytes)
                }
                guard let chunk = try handle.read(upToCount: remaining + 1),
                      !chunk.isEmpty
                else {
                    break
                }
                data.append(chunk)
                if data.count > maxBytes {
                    throw PrivateStateFileReadError.tooLarge(limit: maxBytes)
                }
            }
        } catch let error as PrivateStateFileReadError {
            throw error
        } catch {
            throw PrivateStateFileReadError.readFailed(error.localizedDescription)
        }
        return data
    }
}
