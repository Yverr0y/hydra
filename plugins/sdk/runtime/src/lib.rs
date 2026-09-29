//! Embeds the selected scripting interpreter inside the Wasm guest.
use hya_plugin_sdk::{ErrorCode, Host, Plugin, PluginError, Resolve, ResolveRequest, Result};
use serde_json::Value;

const SOURCE: &str = include_str!(concat!(env!("OUT_DIR"), "/plugin.source"));
#[derive(Default)]
struct Script;
impl Plugin for Script {
    fn enqueue(&mut self, _: &Host, request: hya_plugin_sdk::ResolveRequest) -> Result<hya_plugin_sdk::EnqueueDecision> {
        decode(evaluate("enqueue",serde_json::to_value(request).map_err(invalid)?)?)
    }
    fn complete(&mut self, _: &Host, request: hya_plugin_sdk::CompleteRequest) -> Result<()> {
        decode(evaluate("complete",serde_json::to_value(request).map_err(invalid)?)?)
    }
    fn process(&mut self, _: &Host, request: hya_plugin_sdk::ProcessRequest) -> Result<hya_plugin_sdk::Processed> {
        decode(evaluate("process",serde_json::to_value(request).map_err(invalid)?)?)
    }
    fn check(&mut self, _: &Host) -> Result<()> {
        let value = evaluate("check", Value::Null)?;
        if let Some(error) = value.get("error") { return Err(serde_json::from_value(error.clone()).map_err(invalid)?); }
        Ok(())
    }
    fn resolve(&mut self, _: &Host, request: ResolveRequest) -> Result<Resolve> {
        let value = evaluate("resolve", serde_json::to_value(request).map_err(invalid)?)?;
        if let Some(error) = value.get("error") {
            return Err(serde_json::from_value(error.clone()).map_err(invalid)?);
        }
        serde_json::from_value(value).map_err(invalid)
    }
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    if let Some(error) = value.get("error") {return Err(serde_json::from_value(error.clone()).map_err(invalid)?);}
    serde_json::from_value(value).map_err(invalid)
}
fn fallback(method: &str) -> Option<Value> {
    match method {"check" | "complete" => Some(Value::Null),"process" => Some(serde_json::json!({"changed":false})),"enqueue" => Some(serde_json::json!({"allow":true})),_ => None}
}
fn invalid(e: impl std::fmt::Display) -> PluginError {
    PluginError::new(ErrorCode::InvalidReply, e.to_string())
}
hya_plugin_sdk::export!(Script);

#[cfg(feature = "javascript")]
fn evaluate(method: &str, request: Value) -> Result<Value> {
    use boa_engine::{js_string, Context, JsNativeError, JsValue, NativeFunction, Source};
    let mut context = Context::default();
    context
        .register_global_callable(
            js_string!("_hydra_call"),
            2,
            NativeFunction::from_fn_ptr(|_, args, context| {
                let name = args
                    .first()
                    .unwrap_or(&JsValue::undefined())
                    .to_string(context)?
                    .to_std_string_escaped();
                let request = args
                    .get(1)
                    .unwrap_or(&JsValue::undefined())
                    .to_string(context)?
                    .to_std_string_escaped();
                let request: Value = serde_json::from_str(&request)
                    .map_err(|e| JsNativeError::typ().with_message(e.to_string()))?;
                let result: Value = Host
                    .call(&name, &request)
                    .map_err(|e| JsNativeError::error().with_message(e.to_string()))?;
                Ok(js_string!(result.to_string()).into())
            }),
        )
        .map_err(invalid)?;
    context
        .eval(Source::from_bytes(include_str!("../../nodejs/hydra.js")))
        .map_err(invalid)?;
    context.eval(Source::from_bytes(SOURCE)).map_err(invalid)?;
    let call = if let Some(default) = fallback(method) {
        format!("JSON.stringify(typeof {method} === 'function' ? ({method}({request}) ?? null) : {default})")
    } else { format!("JSON.stringify({method}({request}))") };
    let result = context.eval(Source::from_bytes(&call)).map_err(invalid)?;
    let text = result
        .to_string(&mut context)
        .map_err(invalid)?
        .to_std_string_escaped();
    serde_json::from_str(&text).map_err(invalid)
}

#[cfg(feature = "python")]
fn evaluate(method: &str, request: Value) -> Result<Value> {
    use rustpython_vm::{py_serde, Interpreter, PyObjectRef, PyResult, VirtualMachine};
    fn call(name: String, request: PyObjectRef, vm: &VirtualMachine) -> PyResult<PyObjectRef> {
        let req = serde_json::to_value(py_serde::PyObjectSerializer::new(vm, &request))
            .map_err(|e| vm.new_value_error(e.to_string()))?;
        let reply: Value = Host
            .call(&name, &req)
            .map_err(|e| vm.new_runtime_error(e.to_string()))?;
        py_serde::deserialize(vm, reply).map_err(|e| vm.new_value_error(e.to_string()))
    }
    let interpreter = Interpreter::without_stdlib(Default::default());
    interpreter.enter(|vm| {
        let run = || -> PyResult<Value> {
            let scope = vm.new_scope_with_builtins();
            scope.globals.set_item(
                "_hydra_call",
                vm.new_function("_hydra_call", call).into(),
                vm,
            )?;
            vm.run_string(
                scope.clone(),
                include_str!("../../python/hydra.py"),
                "hydra.py",
            )?;
            vm.run_string(scope.clone(), SOURCE, "plugin.py")?;
            scope.globals.set_item(
                "__request",
                py_serde::deserialize(vm, request)
                    .map_err(|e| vm.new_value_error(e.to_string()))?,
                vm,
            )?;
            scope.globals.set_item("__default",py_serde::deserialize(vm,fallback(method).unwrap_or(Value::Null)).map_err(|e| vm.new_value_error(e.to_string()))?,vm)?;
            let expression = if method == "check" {"check() if 'check' in globals() else None".to_string()} else if fallback(method).is_some() {format!("{method}(__request) if '{method}' in globals() else __default")} else {format!("{method}(__request)")};
            let result = vm.run_block_expr(scope, &expression)?;
            serde_json::to_value(py_serde::PyObjectSerializer::new(vm, &result))
                .map_err(|e| vm.new_value_error(e.to_string()))
        };
        run().map_err(|e| {
            let mut message = String::new();
            let _ = vm.write_exception(&mut message, &e);
            invalid(message)
        })
    })
}
