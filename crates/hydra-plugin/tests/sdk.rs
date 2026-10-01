//! Run an actual SDK module, not a WAT stand-in.
use hya_plugin::manager::Manager;
use hya_plugin_api::ResolveRequest;
#[test]
fn guest_sdk_resolves_through_the_host_abi() {
    let Some(module) = std::env::var_os("HYDRA_TEST_PLUGIN_WASM") else {
        return;
    };
    let package = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::copy(module, package.path().join("plugin.wasm")).unwrap();
    std::fs::write(
        package.path().join("hydra-plugin.toml"),
        include_str!("../../../examples/plugins/direct/hydra-plugin.toml"),
    )
    .unwrap();
    let mut manager = Manager::open(root.path().to_path_buf()).unwrap();
    let manifest = Manager::inspect(package.path()).unwrap().manifest;
    manager
        .install(package.path(), manifest.permissions)
        .unwrap();
    let result = manager
        .resolve(
            || hya_net::TcpConnector,
            ResolveRequest {
                url: "https://example.com/file".into(),
                ..Default::default()
            },
            |_| Box::new(Quiet),
        )
        .unwrap()
        .unwrap();
    manager
        .check("example.direct", hya_net::TcpConnector, Box::new(Quiet))
        .unwrap();
    let refreshed = manager
        .refresh_plan(
            "example.direct",
            ResolveRequest {
                url: "https://example.com/file".into(),
                ..Default::default()
            },
            result.1.clone(),
            vec![result.1.tracks[0].id.clone()],
            || hya_net::TcpConnector,
            |_| Box::new(Quiet),
            Default::default(),
        )
        .unwrap();
    assert_eq!(refreshed.tracks[0].sources, result.1.tracks[0].sources);
    assert_eq!(result.0, "example.direct");
    assert_eq!(
        result.1.tracks[0].sources[0].url,
        "https://example.com/file"
    );
}
struct Quiet;
impl hya_plugin::host::Frontend for Quiet {
    fn prompt(
        &mut self,
        _: hya_plugin_api::Form,
    ) -> Result<hya_plugin_api::Answers, hya_plugin_api::PluginError> {
        unreachable!()
    }
    fn log(&mut self, _: &str, _: &str) {}
    fn progress(&mut self, _: u64, _: Option<u64>, _: Option<&str>) {}
}

#[test]
fn calibrate_reference_parser_on_four_mib() {
    let Some(module) = std::env::var_os("HYDRA_FUEL_PLUGIN_WASM") else {
        return;
    };
    let runtime = hya_plugin::runtime::Runtime::new();
    let compiled = runtime
        .compile(&std::fs::read(module).unwrap(), 64)
        .unwrap();
    let request = serde_json::to_vec(&serde_json::json!({"url":"https://example.com/file","headers":{"fixture":"a".repeat(4*1024*1024)}})).unwrap();
    let output = runtime.call(
        &compiled,
        Box::new(hya_plugin::host::Dispatcher::new(
            hya_plugin::host::HostState::new(
                "example.direct",
                Default::default(),
                hya_net::TcpConnector,
            ),
        )),
        &hya_plugin::runtime::CallCtl::new(std::time::Duration::from_secs(120)),
        "resolve",
        &request,
    );
    output.result.unwrap();
    println!(
        "4 MiB reference parser: {} fuel units; configured {}",
        output.fuel_used,
        hya_plugin_api::limits::FUEL_PER_CALL
    );
    assert!(output.fuel_used * 10 < hya_plugin_api::limits::FUEL_PER_CALL);
}
