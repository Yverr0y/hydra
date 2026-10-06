//! Validates browser links before passing HTTPS packages to permission review.

pub(crate) fn package_url(link: &str) -> Result<String, String> {
    let link = url::Url::parse(link).map_err(|error| error.to_string())?;
    if link.scheme() != "hydra"
        || link.host_str() != Some("install-plugin")
        || !matches!(link.path(), "" | "/")
        || !link.username().is_empty()
        || link.password().is_some()
        || link.port().is_some()
        || link.fragment().is_some()
    {
        return Err("invalid plugin install link".into());
    }
    let mut pairs = link.query_pairs();
    let (key, value) = pairs.next().ok_or("plugin install link needs a URL")?;
    if key != "url" || pairs.next().is_some() {
        return Err("plugin install link needs exactly one URL".into());
    }
    let package = url::Url::parse(&value).map_err(|error| error.to_string())?;
    if package.scheme() != "https"
        || package.host_str().is_none()
        || !package.username().is_empty()
        || package.password().is_some()
        || package.fragment().is_some()
        || !package.path().to_ascii_lowercase().ends_with(".hyaplugin")
    {
        return Err("plugin install links require an HTTPS .hyaplugin URL".into());
    }
    Ok(package.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_encoded_package_queries() {
        assert_eq!(
            package_url("hydra://install-plugin?url=https%3A%2F%2Fexample.com%2Fvideo.hyaplugin%3Ftoken%3Da%26version%3D1").unwrap(),
            "https://example.com/video.hyaplugin?token=a&version=1"
        );
    }

    #[test]
    fn refuses_malformed_actions_and_unsafe_sources() {
        for link in [
            "invalid",
            "https://install-plugin?url=https://example.com/a.hyaplugin",
            "hydra://other?url=https://example.com/a.hyaplugin",
            "hydra://install-plugin/path?url=https://example.com/a.hyaplugin",
            "hydra://user@install-plugin?url=https://example.com/a.hyaplugin",
            "hydra://install-plugin:99?url=https://example.com/a.hyaplugin",
            "hydra://install-plugin?url=https://example.com/a.hyaplugin#fragment",
            "hydra://install-plugin",
            "hydra://install-plugin?other=https://example.com/a.hyaplugin",
            "hydra://install-plugin?url=https://example.com/a.hyaplugin&url=https://example.com/b.hyaplugin",
            "hydra://install-plugin?url=invalid",
            "hydra://install-plugin?url=http://example.com/a.hyaplugin",
            "hydra://install-plugin?url=file:///tmp/a.hyaplugin",
            "hydra://install-plugin?url=https://user:pass@example.com/a.hyaplugin",
            "hydra://install-plugin?url=https://example.com/a.exe",
            "hydra://install-plugin?url=https%3A%2F%2Fexample.com%2Fa.hyaplugin%23fragment",
        ] {
            assert!(package_url(link).is_err(), "{link}");
        }
    }
}
