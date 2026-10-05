//! Publisher signatures for validated plugin archives.
use std::{fs, io::Cursor, io::Write, path::Path, path::PathBuf};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::Args;
use hya_plugin::package::{self, Package, MANIFEST_NAME, SIGNATURE_NAME, SUMS_NAME};
use minisign::{PublicKey, SecretKey, SecretKeyBox};
use zip::{write::SimpleFileOptions, ZipWriter};

#[derive(Args)]
pub(crate) struct SignArgs {
    /// Already built, unsigned .hyaplugin package.
    pub(crate) path: PathBuf,
    /// Minisign private-key file.
    #[arg(long, conflicts_with = "key_env", required_unless_present = "key_env")]
    secret_key: Option<PathBuf>,
    /// Environment variable containing the complete Minisign private key.
    #[arg(long, required_unless_present = "secret_key")]
    key_env: Option<String>,
    /// Minisign public-key file; must match the private key.
    #[arg(long)]
    public_key: PathBuf,
    /// Environment variable containing the encrypted key's password.
    #[arg(long)]
    password_env: Option<String>,
    /// Signed output archive; must differ from the input.
    #[arg(short, long)]
    output: PathBuf,
}

fn key_variable(name: &str) -> Result<String> {
    let value =
        std::env::var(name).with_context(|| format!("missing key environment variable: {name}"))?;
    if value.trim().is_empty() {
        bail!("empty key environment variable: {name}");
    }
    Ok(value)
}

pub(crate) fn sign(args: SignArgs) -> Result<PathBuf> {
    if args.output == args.path
        || (args.output.exists()
            && fs::canonicalize(&args.output)? == fs::canonicalize(&args.path)?)
    {
        bail!("signed output must differ from the input package");
    }
    if !args.path.is_file() {
        bail!("sign requires an unsigned .hyaplugin archive, not a development directory");
    }
    let package = super::validate(&args.path)?;
    let key = match (&args.secret_key, &args.key_env) {
        (Some(path), None) => fs::read_to_string(path).context("read private-key file")?,
        (None, Some(name)) => key_variable(name)?,
        _ => bail!("provide exactly one of --secret-key or --key-env"),
    };
    let password = args.password_env.as_deref().map(key_variable).transpose()?;
    let signed = signed_package(package, &key, &args.public_key, password)?;
    let parent = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    pending.write_all(&signed)?;
    pending.persist(&args.output)?;
    Ok(args.output)
}

fn signed_package(
    package: Package,
    key: &str,
    public_key: &Path,
    password: Option<String>,
) -> Result<Vec<u8>> {
    if package.manifest.publisher_key.is_some() {
        bail!("input package already declares a publisher key; rebuild an unsigned package first");
    }
    let public = PublicKey::from_file(public_key).context("read Minisign public key")?;
    let secret = read_secret(key, password)?;
    let mut entries = package.entries;
    entries.remove(SIGNATURE_NAME);
    let manifest = std::str::from_utf8(entries.get(MANIFEST_NAME).context("missing manifest")?)?;
    entries.insert(
        MANIFEST_NAME.into(),
        format!(
            "publisher_key = {}\n{manifest}",
            toml::Value::String(public.to_base64())
        )
        .into_bytes(),
    );
    let sums = entries
        .iter()
        .filter(|(name, _)| name.as_str() != SUMS_NAME)
        .map(|(name, data)| format!("{}  {name}\n", package::sha256_hex(data)))
        .collect::<String>();
    let signature = minisign::sign(
        Some(&public),
        &secret,
        Cursor::new(sums.as_bytes()),
        None,
        None,
    )
    .context("sign checksums; public and private keys must match")?;
    entries.insert(SUMS_NAME.into(), sums.into_bytes());
    entries.insert(SIGNATURE_NAME.into(), signature.to_string().into_bytes());
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        archive.start_file(
            name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )?;
        archive.write_all(&data)?;
    }
    let signed = archive.finish()?.into_inner();
    package::open(&signed, None).context("verify signed package")?;
    Ok(signed)
}

fn read_secret(key: &str, password: Option<String>) -> Result<SecretKey> {
    let encoded = key
        .lines()
        .nth(1)
        .context("missing encoded Minisign private key")?;
    let raw = STANDARD
        .decode(encoded)
        .context("invalid Minisign private-key encoding")?;
    if raw.len() != 158 || &raw[..2] != b"Ed" || &raw[4..6] != b"B2" {
        bail!("unsupported Minisign private-key format");
    }
    // Minisign -G -W leaves the unencrypted key's checksum zero; signing verifies its public key.
    if raw[2..4] == [0, 0] && raw[126..].iter().all(|byte| *byte == 0) {
        return SecretKey::from_bytes(&raw).context("read unencrypted Minisign private key");
    }
    let boxed = SecretKeyBox::from_string(key)?;
    match boxed.clone().into_unencrypted_secret_key() {
        Ok(secret) => Ok(secret),
        Err(_) => boxed
            .into_secret_key(Some(password.unwrap_or_default()))
            .context("decrypt private key; use --password-env for an encrypted key"),
    }
}
