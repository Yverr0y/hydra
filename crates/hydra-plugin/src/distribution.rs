//! Bounded HTTPS package downloads and explicitly configured signed indexes.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_plugin_api::{ErrorCode, PluginError};
use serde::{Deserialize, Serialize};

use crate::{http, matcher::HostList, package};

fn error(e: impl std::fmt::Display) -> PluginError {
    PluginError::new(ErrorCode::InvalidInput, e.to_string())
}

/// An inspected installation source kept alive until consent is applied.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// Local source passed to the installer after consent.
    pub path: PathBuf,
    temporary: Option<Arc<tempfile::TempPath>>,
}
impl Prepared {
    /// Fetches HTTPS sources or inspects local packages without installing them.
    pub fn new(source: &str, sha256: Option<&str>) -> Result<Self, PluginError> {
        let prepared = if source.starts_with("https://") {
            let bytes = fetch(source)?;
            package::open(&bytes, sha256).map_err(error)?;
            let mut file = tempfile::NamedTempFile::new().map_err(error)?;
            std::io::Write::write_all(&mut file, &bytes).map_err(error)?;
            let path = Arc::new(file.into_temp_path());
            Self {
                path: path.to_path_buf(),
                temporary: Some(path),
            }
        } else {
            if source.contains("://") {
                return Err(error("plugin downloads require HTTPS"));
            }
            let path = PathBuf::from(source);
            if let Some(hash) = sha256 {
                if path.is_dir() {
                    return Err(error("a directory has no archive checksum"));
                }
                package::open(&std::fs::read(&path).map_err(error)?, Some(hash)).map_err(error)?;
            }
            Self {
                path,
                temporary: None,
            }
        };
        crate::manager::Manager::inspect(&prepared.path)?;
        Ok(prepared)
    }
    /// Whether this source was fetched over HTTPS.
    pub fn remote(&self) -> bool {
        self.temporary.is_some()
    }
}

/// Fetches a public HTTPS distribution document through Hydra's transport.
pub fn fetch(address: &str) -> Result<Vec<u8>, PluginError> {
    let url = url::Url::parse(address).map_err(error)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(
            "distribution URLs must use HTTPS without credentials",
        ));
    }
    let connector = hya_net::tls::TlsCapableConnector::new().map_err(error)?;
    let mut policy = http::HttpPolicy {
        proxy: None,
        http: HostList::parse(&["*".into()]).map_err(error)?,
        cookies: HostList::parse(&[]).map_err(error)?,
        user_cookies: hya_net::CookieJar::new(),
        session: hya_net::CookieJar::new(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(error)?;
    let response = runtime.block_on(http::perform(
        &connector,
        &mut policy,
        &http::HttpRequest {
            method: "GET".into(),
            url: address.into(),
            headers: Default::default(),
            body_b64: None,
        },
    ))?;
    if response.status != 200 || !response.url.starts_with("https://") {
        return Err(error(format!(
            "distribution download returned {}",
            response.status
        )));
    }
    hya_net::base64::decode(&response.body_b64).ok_or_else(|| error("invalid response body"))
}

pub(crate) fn compatible(api: i32, minimum: Option<&str>) -> Result<(), PluginError> {
    if api != hya_plugin_api::abi::API_MAJOR {
        return Err(error("unsupported plugin API"));
    }
    if let Some(minimum) = minimum {
        let required = semver::Version::parse(minimum).map_err(error)?;
        let current = semver::Version::parse(env!("HYDRA_PRODUCT_VERSION")).map_err(error)?;
        if required > current {
            return Err(error(format!(
                "plugin requires Hydra {required}; installed {current}"
            )));
        }
    }
    Ok(())
}

/// A user-pinned index location and optional signature verification key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSource {
    pub url: String,
    pub key: Option<String>,
}
/// A publisher's distribution catalog; Hydra supplies no default catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub schema: u32,
    pub name: String,
    pub plugins: Vec<Entry>,
}
/// A package advertised by an index, pinned to its exact archive digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub package: String,
    pub sha256: String,
    pub publisher_key: Option<String>,
    pub api: i32,
    pub min_hydra: Option<String>,
    pub summary: Option<String>,
}
impl IndexSource {
    /// Downloads an index, verifying its signature against the user's pinned key.
    pub fn load(&self) -> Result<Index, PluginError> {
        let bytes = fetch(&self.url)?;
        let signature = if self.key.is_some() {
            Some(fetch(&format!("{}.minisig", self.url))?)
        } else {
            None
        };
        parse_index(&bytes, self.key.as_deref(), signature.as_deref())
    }
}
/// Parses a bounded index and verifies a detached signature when a key is pinned.
pub fn parse_index(
    bytes: &[u8],
    key: Option<&str>,
    signature: Option<&[u8]>,
) -> Result<Index, PluginError> {
    if bytes.len() > 1024 * 1024 {
        return Err(error("index exceeds 1 MiB"));
    }
    if let Some(key) = key {
        let signature = signature.ok_or_else(|| error("signed index has no signature"))?;
        let signature =
            minisign_verify::Signature::decode(std::str::from_utf8(signature).map_err(error)?)
                .map_err(error)?;
        let key = minisign_verify::PublicKey::from_base64(key).map_err(error)?;
        key.verify(bytes, &signature, false).map_err(error)?;
    }
    let index: Index = toml::from_str(std::str::from_utf8(bytes).map_err(error)?).map_err(error)?;
    if index.schema != 1 || index.name.trim().is_empty() {
        return Err(error("unsupported or unnamed index"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for entry in &index.plugins {
        if !ids.insert(&entry.id)
            || entry.sha256.len() != 64
            || !entry.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || !entry.package.starts_with("https://")
        {
            return Err(error("invalid or duplicate index entry"));
        }
        semver::Version::parse(&entry.version).map_err(error)?;
        if let Some(minimum) = &entry.min_hydra {
            semver::Version::parse(minimum).map_err(error)?;
        }
    }
    Ok(index)
}
/// Reads only explicitly added index sources.
pub fn sources(root: &Path) -> Result<Vec<IndexSource>, PluginError> {
    match std::fs::read(root.join("indexes.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(error),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(error(e)),
    }
}
/// Adds a verified index, or replaces its explicitly supplied verification key.
pub fn add(root: &Path, source: IndexSource) -> Result<Index, PluginError> {
    let index = source.load()?;
    let mut list = sources(root)?;
    list.retain(|s| s.url != source.url);
    list.push(source);
    crate::manager::write_atomic(
        &root.join("indexes.json"),
        &serde_json::to_vec(&list).map_err(error)?,
    )?;
    Ok(index)
}
/// Removes an index source without changing any installed plugin.
pub fn remove(root: &Path, url: &str) -> Result<(), PluginError> {
    let mut list = sources(root)?;
    list.retain(|s| s.url != url);
    crate::manager::write_atomic(
        &root.join("indexes.json"),
        &serde_json::to_vec(&list).map_err(error)?,
    )
}

impl Entry {
    /// Fetches exactly the advertised package, including its publisher and version.
    pub fn prepare(&self) -> Result<Prepared, PluginError> {
        compatible(self.api, self.min_hydra.as_deref())?;
        let prepared = Prepared::new(&self.package, Some(&self.sha256))?;
        let package = crate::manager::Manager::inspect(&prepared.path)?;
        if package.manifest.id != self.id
            || package.manifest.version != self.version
            || package.manifest.api != self.api
            || self
                .publisher_key
                .as_ref()
                .is_some_and(|key| package.manifest.publisher_key.as_ref() != Some(key))
        {
            return Err(error(
                "package identity or publisher differs from its index entry",
            ));
        }
        Ok(prepared)
    }
}
/// Checks configured catalogs for newer compatible versions, without installing them.
pub fn updates(
    root: &Path,
    installed: &[crate::manager::Installed],
    id: Option<&str>,
) -> Result<Vec<Entry>, PluginError> {
    let mut updates = Vec::new();
    for source in sources(root)? {
        for entry in source.load()?.plugins {
            if compatible(entry.api, entry.min_hydra.as_deref()).is_err()
                || id.is_some_and(|id| id != entry.id)
            {
                continue;
            }
            if let Some(previous) = installed.iter().find(|p| p.manifest.id == entry.id) {
                if semver::Version::parse(&entry.version).map_err(error)?
                    > semver::Version::parse(&previous.manifest.version).map_err(error)?
                {
                    updates.push(entry);
                }
            }
        }
    }
    Ok(updates)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_refuses_missing_pinned_signature_bad_hash_and_duplicate_ids() {
        let text = format!("schema=1\nname='Test'\n[[plugins]]\nid='a.b'\nname='Example'\nversion='1.2.0'\npackage='https://example.com/plugin.hyaplugin'\nsha256='{}'\napi=1\n", "a".repeat(64));
        assert!(parse_index(text.as_bytes(), None, None).is_ok());
        assert!(parse_index(text.as_bytes(), Some("pinned"), None).is_err());
        assert!(parse_index(text.replace(&"a".repeat(64), "bad").as_bytes(), None, None).is_err());
        let duplicate = format!("{text}{}", &text[text.find("[[plugins]]").unwrap()..]);
        assert!(parse_index(duplicate.as_bytes(), None, None).is_err());
        assert!(Prepared::new("http://example.com/plugin.hyaplugin", None).is_err());
    }
    #[test]
    fn signed_index_authenticates_content_key_and_trusted_comment() {
        let data = include_bytes!("../tests/fixtures/index.toml");
        let key = include_str!("../tests/fixtures/index.pub").trim();
        let signature = include_bytes!("../tests/fixtures/index.minisig");
        let index = parse_index(data, Some(key), Some(signature)).unwrap();
        assert_eq!(index.plugins[0].id, "example.direct");
        let mut changed = data.to_vec();
        changed.extend_from_slice(b"\n#tampered");
        assert!(parse_index(&changed, Some(key), Some(signature)).is_err());
        let comment = String::from_utf8_lossy(signature).replace("Hydra test fixture", "different");
        assert!(parse_index(data, Some(key), Some(comment.as_bytes())).is_err());
        assert!(parse_index(data, Some("invalid"), Some(signature)).is_err());
        assert!(parse_index(data, Some(key), Some(b"invalid")).is_err());
    }
    #[test]
    fn catalogs_bound_input_and_enforce_host_compatibility() {
        assert!(parse_index(&vec![b'x'; 1024 * 1024 + 1], None, None).is_err());
        assert!(parse_index(b"schema=2\nname='Future'", None, None).is_err());
        assert!(parse_index(b"schema=1\nname=' '", None, None).is_err());
        assert!(parse_index(b"invalid", None, None).is_err());
        assert!(compatible(1, Some(env!("HYDRA_PRODUCT_VERSION"))).is_ok());
        assert!(compatible(1, None).is_ok());
        assert!(compatible(2, None).is_err());
        assert!(compatible(1, Some("999.0.0")).is_err());
        assert!(compatible(1, Some("broken")).is_err());
        for url in [
            "http://example.com/plugin",
            "https://user:pass@example.com/plugin",
            "file:///tmp/plugin",
            "not a URL",
            "https://127.0.0.1/plugin",
        ] {
            assert!(fetch(url).is_err());
        }
    }
    #[test]
    fn index_source_removal_preserves_other_sources_and_rejects_corrupt_state() {
        let root = tempfile::tempdir().unwrap();
        assert!(sources(root.path()).unwrap().is_empty());
        let list = vec![
            IndexSource {
                url: "https://example.com/a".into(),
                key: Some("pin".into()),
            },
            IndexSource {
                url: "https://example.com/b".into(),
                key: None,
            },
        ];
        std::fs::write(
            root.path().join("indexes.json"),
            serde_json::to_vec(&list).unwrap(),
        )
        .unwrap();
        remove(root.path(), "https://example.com/a").unwrap();
        let left = sources(root.path()).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].url, list[1].url);
        std::fs::write(root.path().join("indexes.json"), b"bad").unwrap();
        assert!(sources(root.path()).is_err());
    }
}
