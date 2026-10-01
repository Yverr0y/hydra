//! Minimal reference resolver used by the SDK ABI integration test.
use hya_plugin_sdk::{Host, Plan, Plugin, Resolve, ResolveRequest, Track};
#[derive(Default)]
struct Direct;
impl Plugin for Direct {
    fn resolve(&mut self, _: &Host, request: ResolveRequest) -> hya_plugin_sdk::Result<Resolve> {
        Ok(Resolve::Plan(Plan::single(
            "Reference download",
            Track::file("file", request.url),
        )))
    }
}
hya_plugin_sdk::export!(Direct);
