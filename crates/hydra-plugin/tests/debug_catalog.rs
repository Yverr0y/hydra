//! Exercises the debug catalog override in its own process, isolated from unit tests.

#[test]
#[cfg(debug_assertions)]
fn debug_catalog_is_configured_once_and_only_its_http_origin_is_reachable() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use hya_plugin::distribution::{configure_debug_catalog, fetch, sources};

    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", server.local_addr().unwrap());
    let address = format!("{origin}/plugins.json");
    let outside = TcpListener::bind("127.0.0.1:0").unwrap();
    outside.set_nonblocking(true).unwrap();
    let redirect = format!("HTTP/1.1 302 Found\r\nLocation: http://{}/other.json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", outside.local_addr().unwrap());
    let responding = std::thread::spawn(move || {
        for response in [
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_string(),
            redirect,
        ] {
            let (mut client, _) = server.accept().unwrap();
            let mut request = [0; 4096];
            let size = client.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("GET /"));
            client.write_all(response.as_bytes()).unwrap();
        }
    });
    configure_debug_catalog(&address).unwrap();
    assert!(configure_debug_catalog(&address).is_err());
    let root = tempfile::tempdir().unwrap();
    let configured = sources(root.path()).unwrap();
    assert_eq!(configured[0].url, address);
    assert!(configured[0].is_default());
    assert_eq!(fetch(&address).unwrap(), b"{}");
    assert!(fetch(&format!("{origin}/redirect")).is_err());
    assert_eq!(
        outside.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    responding.join().unwrap();
    assert!(fetch("http://127.0.0.1:1/other.json").is_err());
    assert!(fetch("http://example.com/plugins.json").is_err());
}
