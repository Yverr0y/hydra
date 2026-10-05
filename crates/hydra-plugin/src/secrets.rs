//! Private persisted settings, protected by file permissions or Windows DPAPI.
use hya_plugin_api::{PluginError, Value};
use std::{collections::BTreeMap, path::Path};

pub(crate) fn read(path: &Path) -> Result<BTreeMap<String, Value>, PluginError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(error(e)),
    };
    #[cfg(windows)]
    let bytes = crypt(&bytes, false).map_err(error)?;
    serde_json::from_slice(&bytes).map_err(error)
}
pub(crate) fn write(path: &Path, values: &BTreeMap<String, Value>) -> Result<(), PluginError> {
    let bytes = serde_json::to_vec(values).map_err(error)?;
    #[cfg(windows)]
    let bytes = crypt(&bytes, true).map_err(error)?;
    crate::manager::write_atomic(path, &bytes)
}
fn error(e: impl std::fmt::Display) -> PluginError {
    PluginError::new(
        hya_plugin_api::ErrorCode::Internal,
        format!("plugin secrets: {e}"),
    )
}
#[cfg(windows)]
fn crypt(data: &[u8], encrypt: bool) -> std::io::Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).map_err(std::io::Error::other)?,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: input remains live; DPAPI owns and initializes the output on success.
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: DPAPI returned this valid cbData-sized allocation.
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    // SAFETY: this is the allocation returned by DPAPI, freed once after copying.
    unsafe { LocalFree(output.pbData as _) };
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_round_trip_privately_and_reject_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.bin");
        assert!(read(&path).unwrap().is_empty());
        let expected = [("token".into(), Value::Text("test credential".into()))]
            .into_iter()
            .collect();
        write(&path, &expected).unwrap();
        assert_eq!(read(&path).unwrap(), expected);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(path, b"corrupt").unwrap();
        assert!(read(&dir.path().join("secrets.bin")).is_err());
    }
}
