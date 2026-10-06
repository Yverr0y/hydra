//! `.hyaplugin` packages: bounded extraction in memory, checksum and signature
//! verification. Nothing here touches the filesystem except `write_into`.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Cursor, Read};
use std::path::Path;

use hya_plugin_api::abi::API_MAJOR;
use hya_plugin_api::limits::{
    MAX_ICON, MAX_MANIFEST, MAX_MODULE, MAX_PACKAGE_ARCHIVE, MAX_PACKAGE_ENTRIES,
    MAX_PACKAGE_UNPACKED, MAX_README,
};
use hya_plugin_api::Manifest;
use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};

pub const MANIFEST_NAME: &str = "hydra-plugin.toml";
pub const SUMS_NAME: &str = "SHA256SUMS";
pub const SIGNATURE_NAME: &str = "SHA256SUMS.minisig";
const MAX_SIGNATURE: u64 = 4 * 1024;
const WASM_MAGIC: &[u8] = b"\0asm";

/// Why a package was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageError(pub String);

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PackageError {}

fn refuse<T>(msg: impl Into<String>) -> Result<T, PackageError> {
    Err(PackageError(msg.into()))
}

/// Whether a package proves who made it.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Signing {
    #[default]
    Unsigned,
    /// The signature over `SHA256SUMS` verified against this publisher key.
    Verified { fingerprint: String },
}

/// A verified package, held in memory.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub module: Vec<u8>,
    pub entries: BTreeMap<String, Vec<u8>>,
    pub archive_sha256: String,
    pub signing: Signing,
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// A short stable identifier for a publisher key, shown when keys differ.
pub fn key_fingerprint(publisher_key: &str) -> String {
    sha256_hex(publisher_key.trim().as_bytes())[..16].to_string()
}

fn allowed_cap(name: &str, module: Option<&str>) -> Option<u64> {
    match name {
        MANIFEST_NAME => Some(MAX_MANIFEST),
        SUMS_NAME => Some(MAX_MANIFEST),
        SIGNATURE_NAME => Some(MAX_SIGNATURE),
        "icon.png" => Some(MAX_ICON),
        "README.md" | "LICENSE" => Some(MAX_README),
        n if Some(n) == module => Some(MAX_MODULE),
        _ => None,
    }
}

fn valid_entry_name(name: &str) -> Result<(), PackageError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || name
            .split('/')
            .any(|c| c == ".." || c == "." || c.is_empty())
    {
        return refuse(format!("entry `{name}` has an unsafe name"));
    }
    if name.contains('/') {
        return refuse(format!("entry `{name}` is not at the package root"));
    }
    Ok(())
}

fn read_bounded(
    file: &mut impl Read,
    cap: u64,
    total: &mut u64,
    name: &str,
) -> Result<Vec<u8>, PackageError> {
    let mut buf = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut buf)
        .map_err(|e| PackageError(format!("entry `{name}`: {e}")))?;
    if buf.len() as u64 > cap {
        return refuse(format!("entry `{name}` exceeds its size cap"));
    }
    *total += buf.len() as u64;
    if *total > MAX_PACKAGE_UNPACKED {
        return refuse("package is too large when unpacked");
    }
    Ok(buf)
}

fn parse_sums(text: &str) -> Result<BTreeMap<String, String>, PackageError> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (digest, name) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| PackageError(format!("malformed {SUMS_NAME} line `{line}`")))?;
        let name = name.trim_start().trim_start_matches('*');
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return refuse(format!("{SUMS_NAME} digest for `{name}` is malformed"));
        }
        if map
            .insert(name.to_string(), digest.to_ascii_lowercase())
            .is_some()
        {
            return refuse(format!("{SUMS_NAME} lists `{name}` twice"));
        }
    }
    Ok(map)
}

/// Opens, bounds-checks and verifies a package.
///
/// `expected_sha256` pins the archive hash when the caller has one.
///
/// # Errors
/// The first rule the archive breaks, naming the offending entry.
pub fn open(bytes: &[u8], expected_sha256: Option<&str>) -> Result<Package, PackageError> {
    if bytes.len() as u64 > MAX_PACKAGE_ARCHIVE {
        return refuse("package archive is over 16 MiB");
    }
    let archive_sha256 = sha256_hex(bytes);
    if let Some(want) = expected_sha256 {
        if !want.trim().eq_ignore_ascii_case(&archive_sha256) {
            return refuse("package sha256 does not match the expected value");
        }
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| PackageError(format!("not a valid package: {e}")))?;
    if zip.len() > MAX_PACKAGE_ENTRIES {
        return refuse("package has too many entries");
    }

    let mut names = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let f = zip
            .by_index_raw(i)
            .map_err(|e| PackageError(format!("entry {i}: {e}")))?;
        let name = f.name().to_string();
        valid_entry_name(&name)?;
        if f.is_dir() || f.is_symlink() || f.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return refuse(format!("entry `{name}` is a directory or link"));
        }
        if names.contains(&name) {
            return refuse(format!("entry `{name}` appears twice"));
        }
        names.push(name);
    }

    let mut total = 0u64;
    let mut entries = BTreeMap::new();
    let manifest_idx = names
        .iter()
        .position(|n| n == MANIFEST_NAME)
        .ok_or_else(|| PackageError(format!("{MANIFEST_NAME} is missing")))?;
    let manifest_bytes = {
        let mut f = zip
            .by_index(manifest_idx)
            .map_err(|e| PackageError(e.to_string()))?;
        read_bounded(&mut f, MAX_MANIFEST, &mut total, MANIFEST_NAME)?
    };
    let manifest: Manifest = toml::from_str(
        std::str::from_utf8(&manifest_bytes)
            .map_err(|_| PackageError(format!("{MANIFEST_NAME} is not UTF-8")))?,
    )
    .map_err(|e| PackageError(format!("{MANIFEST_NAME}: {e}")))?;
    manifest.validate().map_err(PackageError)?;
    semver::Version::parse(&manifest.version).map_err(|e| PackageError(e.to_string()))?;
    if let Some(minimum) = &manifest.min_hydra {
        semver::Version::parse(minimum).map_err(|e| PackageError(e.to_string()))?;
    }
    if manifest.api != API_MAJOR {
        return refuse(format!(
            "plugin targets API {}, this Hydra speaks {API_MAJOR}",
            manifest.api
        ));
    }
    entries.insert(MANIFEST_NAME.to_string(), manifest_bytes);

    for (i, name) in names.iter().enumerate() {
        if i == manifest_idx {
            continue;
        }
        let Some(cap) = allowed_cap(name, Some(&manifest.module)).or_else(|| {
            manifest
                .native_modules
                .iter()
                .any(|m| m.module == *name)
                .then_some(MAX_MODULE)
        }) else {
            return refuse(format!("entry `{name}` is not allowed in a package"));
        };
        let mut f = zip.by_index(i).map_err(|e| PackageError(e.to_string()))?;
        entries.insert(name.clone(), read_bounded(&mut f, cap, &mut total, name)?);
    }

    for native in &manifest.native_modules {
        if !entries.contains_key(&native.module) {
            return refuse(format!("native module `{}` is missing", native.module));
        }
    }
    let module = entries
        .get(&manifest.module)
        .cloned()
        .ok_or_else(|| PackageError(format!("module `{}` is missing", manifest.module)))?;
    if !module.starts_with(WASM_MAGIC) {
        return refuse("module is not a WebAssembly binary");
    }

    let signing = verify(&manifest, &entries)?;
    Ok(Package {
        manifest,
        module,
        entries,
        archive_sha256,
        signing,
    })
}

/// Checks `SHA256SUMS` against the entries and its signature, if keyed.
fn verify(
    manifest: &Manifest,
    entries: &BTreeMap<String, Vec<u8>>,
) -> Result<Signing, PackageError> {
    let sums_raw = entries
        .get(SUMS_NAME)
        .ok_or_else(|| PackageError(format!("{SUMS_NAME} is missing")))?;
    let sums_text = std::str::from_utf8(sums_raw)
        .map_err(|_| PackageError(format!("{SUMS_NAME} is not UTF-8")))?;
    let listed = parse_sums(sums_text)?;
    for (name, data) in entries {
        if name == SUMS_NAME || name == SIGNATURE_NAME {
            continue;
        }
        match listed.get(name) {
            None => return refuse(format!("{SUMS_NAME} does not list `{name}`")),
            Some(want) if *want != sha256_hex(data) => {
                return refuse(format!("`{name}` does not match its {SUMS_NAME} digest"))
            }
            Some(_) => {}
        }
    }
    if let Some(extra) = listed.keys().find(|n| !entries.contains_key(*n)) {
        return refuse(format!(
            "{SUMS_NAME} lists `{extra}`, which is not in the package"
        ));
    }

    let Some(key) = &manifest.publisher_key else {
        return Ok(Signing::Unsigned);
    };
    let sig_raw = entries.get(SIGNATURE_NAME).ok_or_else(|| {
        PackageError(format!(
            "publisher_key is set but {SIGNATURE_NAME} is missing"
        ))
    })?;
    let pk = PublicKey::from_base64(key.trim())
        .map_err(|e| PackageError(format!("publisher_key is invalid: {e}")))?;
    let sig = Signature::decode(
        std::str::from_utf8(sig_raw)
            .map_err(|_| PackageError(format!("{SIGNATURE_NAME} is not UTF-8")))?,
    )
    .map_err(|e| PackageError(format!("{SIGNATURE_NAME} is invalid: {e}")))?;
    pk.verify(sums_raw, &sig, false)
        .map_err(|e| PackageError(format!("signature does not verify: {e}")))?;
    Ok(Signing::Verified {
        fingerprint: key_fingerprint(key),
    })
}

impl Package {
    /// Writes every entry into `dir`, which must exist and be empty.
    ///
    /// # Errors
    /// Any I/O error.
    pub fn write_into(&self, dir: &Path) -> std::io::Result<()> {
        for (name, data) in &self.entries {
            std::fs::write(dir.join(name), data)?;
        }
        Ok(())
    }
}

/// Loads a dev-mode directory: manifest and module only, no checksums.
///
/// # Errors
/// The manifest or module is missing, oversized or invalid.
pub fn load_dir(dir: &Path) -> Result<Package, PackageError> {
    let read = |name: &str, cap: u64| -> Result<Vec<u8>, PackageError> {
        let meta =
            std::fs::metadata(dir.join(name)).map_err(|e| PackageError(format!("{name}: {e}")))?;
        if meta.len() > cap {
            return refuse(format!("`{name}` exceeds its size cap"));
        }
        std::fs::read(dir.join(name)).map_err(|e| PackageError(format!("{name}: {e}")))
    };
    let manifest_bytes = read(MANIFEST_NAME, MAX_MANIFEST)?;
    let manifest: Manifest = toml::from_str(
        std::str::from_utf8(&manifest_bytes)
            .map_err(|_| PackageError(format!("{MANIFEST_NAME} is not UTF-8")))?,
    )
    .map_err(|e| PackageError(format!("{MANIFEST_NAME}: {e}")))?;
    manifest.validate().map_err(PackageError)?;
    semver::Version::parse(&manifest.version).map_err(|e| PackageError(e.to_string()))?;
    if let Some(minimum) = &manifest.min_hydra {
        semver::Version::parse(minimum).map_err(|e| PackageError(e.to_string()))?;
    }
    if manifest.api != API_MAJOR {
        return refuse(format!(
            "plugin targets API {}, this Hydra speaks {API_MAJOR}",
            manifest.api
        ));
    }
    let module = read(&manifest.module, MAX_MODULE)?;
    if !module.starts_with(WASM_MAGIC) {
        return refuse("module is not a WebAssembly binary");
    }
    let mut entries = BTreeMap::new();
    entries.insert(MANIFEST_NAME.to_string(), manifest_bytes);
    entries.insert(manifest.module.clone(), module.clone());
    let mut total = entries
        .values()
        .map(|bytes| bytes.len() as u64)
        .sum::<u64>();
    for native in &manifest.native_modules {
        let bytes = read(&native.module, MAX_MODULE)?;
        total += bytes.len() as u64;
        if total > MAX_PACKAGE_UNPACKED {
            return refuse("native package exceeds extraction limit");
        }
        entries.insert(native.module.clone(), bytes);
    }
    Ok(Package {
        archive_sha256: sha256_hex(&module),
        manifest,
        module,
        entries,
        signing: Signing::Unsigned,
    })
}

#[cfg(test)]
pub(crate) mod testing {
    use std::io::Write;

    use super::*;

    pub const WASM: &[u8] = b"\0asm\x01\0\0\0";

    pub fn manifest_toml(extra: &str) -> String {
        format!(
            "id = \"example.direct\"\nname = \"Direct\"\nversion = \"1.0.0\"\napi = 1\nmodule = \"plugin.wasm\"\n{extra}\n"
        )
    }

    pub fn sums(entries: &[(&str, &[u8])]) -> String {
        entries
            .iter()
            .map(|(n, d)| format!("{}  {n}\n", sha256_hex(d)))
            .collect()
    }

    pub fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    pub fn good_entries(manifest: &str) -> Vec<(String, Vec<u8>)> {
        let m = manifest.as_bytes().to_vec();
        let listing = sums(&[(MANIFEST_NAME, &m), ("plugin.wasm", WASM)]);
        vec![
            (MANIFEST_NAME.into(), m),
            ("plugin.wasm".into(), WASM.to_vec()),
            (SUMS_NAME.into(), listing.into_bytes()),
        ]
    }

    pub fn build(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
        let refs: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        zip_of(&refs)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn err(bytes: &[u8]) -> String {
        open(bytes, None).unwrap_err().0
    }

    #[test]
    fn a_well_formed_unsigned_package_opens() {
        let pkg = open(&build(&good_entries(&manifest_toml(""))), None).unwrap();
        assert_eq!(pkg.manifest.id, "example.direct");
        assert_eq!(pkg.module, WASM);
        assert_eq!(pkg.signing, Signing::Unsigned);
        assert_eq!(pkg.archive_sha256.len(), 64);
    }

    #[test]
    fn the_expected_archive_hash_pins_the_file() {
        let bytes = build(&good_entries(&manifest_toml("")));
        let good = sha256_hex(&bytes);
        assert!(open(&bytes, Some(&good)).is_ok());
        assert!(open(&bytes, Some(&"0".repeat(64))).is_err());
    }

    #[test]
    fn unsafe_entry_names_are_refused() {
        for bad in ["../evil", "/abs", "a\\b", "dir/file", "./x", "a//b"] {
            let mut e = good_entries(&manifest_toml(""));
            e.push((bad.into(), vec![1]));
            assert!(
                err(&build(&e)).contains("unsafe") || err(&build(&e)).contains("root"),
                "{bad}"
            );
        }
    }

    #[test]
    fn entries_outside_the_format_are_refused_by_name() {
        let mut e = good_entries(&manifest_toml(""));
        e.push(("extra.sh".into(), vec![1]));
        assert!(err(&build(&e)).contains("extra.sh"));
    }

    #[test]
    fn duplicate_entries_are_refused() {
        let mut e = good_entries(&manifest_toml(""));
        e.push(("README.md".into(), vec![1]));
        e.push(("README.mx".into(), vec![2]));
        let mut bytes = build(&e);
        for i in 0..bytes.len() - 9 {
            if &bytes[i..i + 9] == b"README.mx" {
                bytes[i..i + 9].copy_from_slice(b"README.md");
            }
        }
        assert!(open(&bytes, None).is_err());
    }

    #[test]
    fn an_entry_missing_from_the_sums_is_refused() {
        let mut e = good_entries(&manifest_toml(""));
        e.push(("README.md".into(), b"hi".to_vec()));
        assert!(err(&build(&e)).contains("does not list `README.md`"));
    }

    #[test]
    fn a_tampered_module_fails_its_digest() {
        let mut e = good_entries(&manifest_toml(""));
        e[1].1 = b"\0asm\x01\0\0\0tampered".to_vec();
        assert!(err(&build(&e)).contains("does not match"));
    }

    #[test]
    fn sums_listing_a_ghost_entry_is_refused() {
        let mut e = good_entries(&manifest_toml(""));
        let mut text = String::from_utf8(e[2].1.clone()).unwrap();
        text.push_str(&format!("{}  ghost\n", "a".repeat(64)));
        e[2].1 = text.into_bytes();
        assert!(err(&build(&e)).contains("ghost"));
    }

    #[test]
    fn missing_pieces_are_named() {
        let e = good_entries(&manifest_toml(""));
        let no_manifest: Vec<_> = e
            .iter()
            .filter(|(n, _)| n != MANIFEST_NAME)
            .cloned()
            .collect();
        assert!(err(&build(&no_manifest)).contains(MANIFEST_NAME));
        let no_sums: Vec<_> = e.iter().filter(|(n, _)| n != SUMS_NAME).cloned().collect();
        assert!(err(&build(&no_sums)).contains(SUMS_NAME));
        let no_module: Vec<_> = e
            .iter()
            .filter(|(n, _)| n != "plugin.wasm")
            .cloned()
            .collect();
        assert!(err(&build(&no_module)).contains("plugin.wasm"));
    }

    #[test]
    fn the_module_must_be_wasm_and_the_api_must_match() {
        let manifest = manifest_toml("");
        let m = manifest.as_bytes().to_vec();
        let junk = b"not wasm".to_vec();
        let listing = sums(&[(MANIFEST_NAME, &m), ("plugin.wasm", &junk)]).into_bytes();
        let e = vec![
            (MANIFEST_NAME.to_string(), m),
            ("plugin.wasm".to_string(), junk),
            (SUMS_NAME.to_string(), listing),
        ];
        assert!(err(&build(&e)).contains("WebAssembly"));

        let future = manifest_toml("").replace("api = 1", "api = 2");
        assert!(err(&build(&good_entries(&future))).contains("API 2"));
    }

    #[test]
    fn an_invalid_manifest_is_refused() {
        let bad = manifest_toml("").replace("example.direct", "Bad Id");
        assert!(open(&build(&good_entries(&bad)), None).is_err());
        assert!(open(&build(&good_entries("not = [toml")), None).is_err());
    }

    #[test]
    fn size_caps_apply_during_extraction() {
        let mut e = good_entries(&manifest_toml(""));
        let big = vec![b'x'; (MAX_README + 1) as usize];
        e.push(("README.md".into(), big));
        assert!(err(&build(&e)).contains("size cap"));
    }

    #[test]
    fn too_many_entries_and_oversized_archives_are_refused() {
        let mut e = good_entries(&manifest_toml(""));
        for i in 0..MAX_PACKAGE_ENTRIES {
            e.push((format!("f{i}"), vec![1]));
        }
        assert!(err(&build(&e)).contains("too many"));
        assert!(err(&vec![0u8; (MAX_PACKAGE_ARCHIVE + 1) as usize]).contains("16 MiB"));
        assert!(err(b"not a zip").contains("not a valid package"));
    }

    #[test]
    fn publisher_key_without_a_signature_is_refused() {
        let m = manifest_toml(
            "publisher_key = \"RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\"",
        );
        assert!(err(&build(&good_entries(&m))).contains(SIGNATURE_NAME));
    }

    #[test]
    fn a_bogus_signature_does_not_verify() {
        let m = manifest_toml(
            "publisher_key = \"RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\"",
        );
        let mut e = good_entries(&m);
        e.push((SIGNATURE_NAME.into(), b"junk".to_vec()));
        let msg = err(&build(&e));
        assert!(msg.contains("invalid") || msg.contains("verify"), "{msg}");
    }

    #[test]
    fn fingerprints_differ_per_key() {
        assert_ne!(key_fingerprint("RWQaaaa"), key_fingerprint("RWQbbbb"));
        assert_eq!(key_fingerprint("RWQaaaa"), key_fingerprint(" RWQaaaa\n"));
    }

    #[test]
    fn dev_directory_loads_without_checksums_and_writes_back() {
        let dir = std::env::temp_dir().join(format!("hyp-dev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_NAME), manifest_toml("")).unwrap();
        std::fs::write(dir.join("plugin.wasm"), WASM).unwrap();
        let pkg = load_dir(&dir).unwrap();
        assert_eq!(pkg.module, WASM);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        pkg.write_into(&out).unwrap();
        assert!(out.join("plugin.wasm").exists());
        std::fs::remove_file(dir.join("plugin.wasm")).unwrap();
        assert!(load_dir(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn native_libraries_are_declared_present_and_covered_by_package_checksums() {
        let manifest = manifest_toml(
            "[[native_modules]]\nid='test-engine'\nplatform='linux-x86_64'\nmodule='test.so'\n",
        );
        let mut entries = good_entries(&manifest);
        entries.push(("test.so".into(), b"native-library".to_vec()));
        let sums = sums(
            &entries
                .iter()
                .filter(|(name, _)| name != SUMS_NAME)
                .map(|(name, data)| (name.as_str(), data.as_slice()))
                .collect::<Vec<_>>(),
        );
        entries
            .iter_mut()
            .find(|(name, _)| name == SUMS_NAME)
            .unwrap()
            .1 = sums.into_bytes();
        let package = open(&build(&entries), None).unwrap();
        assert_eq!(package.entries["test.so"], b"native-library");
        entries
            .iter_mut()
            .find(|(name, _)| name == "test.so")
            .unwrap()
            .1 = b"changed".to_vec();
        assert!(open(&build(&entries), None).is_err());
        entries.retain(|(name, _)| name != "test.so");
        assert!(open(&build(&entries), None)
            .unwrap_err()
            .0
            .contains("missing"));
    }
}

/// Builds a checksummed package from a development directory.
///
/// # Errors
/// Invalid input, unreadable module or ZIP encoding errors are returned.
pub fn pack(dir: &Path) -> Result<Vec<u8>, PackageError> {
    use std::io::Write;
    let package = load_dir(dir)?;
    if package.manifest.publisher_key.is_some() {
        return refuse(
            "pack creates unsigned packages; remove publisher_key or sign SHA256SUMS separately",
        );
    }
    let mut entries = package.entries;
    let sums: String = entries
        .iter()
        .map(|(name, bytes)| format!("{}  {name}\n", sha256_hex(bytes)))
        .collect();
    entries.insert(SUMS_NAME.into(), sums.into_bytes());
    let cursor = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(cursor);
    for (name, bytes) in entries {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .map_err(|e| PackageError(e.to_string()))?;
        writer
            .write_all(&bytes)
            .map_err(|e| PackageError(e.to_string()))?;
    }
    let bytes = writer
        .finish()
        .map_err(|e| PackageError(e.to_string()))?
        .into_inner();
    open(&bytes, None)?;
    Ok(bytes)
}
