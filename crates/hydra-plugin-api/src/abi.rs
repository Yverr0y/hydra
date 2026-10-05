//! Symbol names and the API major shared by host and guest.

/// The API major a module built against this crate reports from `hydra_api`.
pub const API_MAJOR: i32 = 1;

pub const EXPORT_API: &str = "hydra_api";
pub const EXPORT_ALLOC: &str = "hydra_alloc";
pub const EXPORT_CALL: &str = "hydra_call";
pub const IMPORT_MODULE: &str = "hydra";
pub const IMPORT_HOST_CALL: &str = "hydra_host_call";

pub const METHOD_RESOLVE: &str = "resolve";
pub const METHOD_REFRESH: &str = "refresh";
pub const METHOD_CHECK: &str = "check";
pub const METHOD_HOOKS: &str = "hooks";

/// Host functions, multiplexed by name over the single import.
pub const HOST_HTTP: &str = "http";
pub const HOST_EXEC: &str = "exec";
pub const HOST_DATA_READ: &str = "data_read";
pub const HOST_DATA_WRITE: &str = "data_write";
pub const HOST_PROMPT: &str = "prompt";
pub const HOST_SETTINGS: &str = "settings";
pub const HOST_STORAGE_GET: &str = "storage_get";
pub const HOST_STORAGE_SET: &str = "storage_set";
pub const HOST_LOG: &str = "log";
pub const HOST_PROGRESS: &str = "progress";

/// Splits the `i64` a call returns into `(ptr, len)`.
pub fn unpack(packed: i64) -> (u32, u32) {
    let bits = packed as u64;
    ((bits >> 32) as u32, bits as u32)
}

/// Packs a reply location the way `hydra_call` returns it.
pub fn pack(ptr: u32, len: u32) -> i64 {
    (((ptr as u64) << 32) | len as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_round_trips_extremes() {
        for (ptr, len) in [(0, 0), (1, 2), (u32::MAX, u32::MAX), (0x8000_0000, 7)] {
            assert_eq!(unpack(pack(ptr, len)), (ptr, len));
        }
    }
}
