//! Limits the host enforces, readable by plugin authors.

pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
pub const MAX_TRACKS: usize = 512;
pub const MAX_SOURCES_PER_TRACK: usize = 16;
pub const MAX_HEADERS_PER_TRACK: usize = 32;
pub const MAX_HEADER_NAME: usize = 256;
pub const MAX_HEADER_VALUE: usize = 8 * 1024;
pub const MAX_URL: usize = 8 * 1024;
pub const MAX_STRING_FIELD: usize = 4 * 1024;
pub const MAX_HTTP_BODY: usize = 16 * 1024 * 1024;
pub const MAX_BATCH: usize = 32;
pub const MAX_EXEC_OUTPUT: usize = 16 * 1024 * 1024;
pub const MAX_STORAGE_TOTAL: usize = 1024 * 1024;
pub const MAX_STORAGE_VALUE: usize = 64 * 1024;
pub const MAX_SESSION_JAR: usize = 256 * 1024;
pub const MAX_DATA_DIR: u64 = 256 * 1024 * 1024;
pub const MAX_PACKAGE_ARCHIVE: u64 = 16 * 1024 * 1024;
pub const MAX_PACKAGE_UNPACKED: u64 = 64 * 1024 * 1024;
pub const MAX_PACKAGE_ENTRIES: usize = 16;
pub const MAX_MANIFEST: u64 = 64 * 1024;
pub const MAX_ICON: u64 = 256 * 1024;
pub const MAX_README: u64 = 1024 * 1024;
pub const MAX_MODULE: u64 = 16 * 1024 * 1024;
pub const DEFAULT_MEMORY_MB: u32 = 64;
pub const MAX_MEMORY_MB: u32 = 256;
pub const MIN_CHUNK_HINT: u64 = 1024 * 1024;
pub const MAX_CHUNK_HINT: u64 = 64 * 1024 * 1024;
pub const HTTP_PER_HOST_IN_FLIGHT: usize = 4;

/// Wasmi budget: the 4 MiB reference parse uses 33,303,247 units; embedded interpreters need more.
pub const FUEL_PER_CALL: u64 = 5_000_000_000;
