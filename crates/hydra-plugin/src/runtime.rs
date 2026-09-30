//! The wasmi runtime: compile and validate a module, run one call in a fresh
//! instance under a fuel budget, a memory cap and a deadline.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hya_plugin_api::abi::{
    pack, unpack, API_MAJOR, EXPORT_ALLOC, EXPORT_API, EXPORT_CALL, IMPORT_HOST_CALL, IMPORT_MODULE,
};
use hya_plugin_api::limits::{FUEL_PER_CALL, MAX_PAYLOAD};
use hya_plugin_api::{ErrorCode, ErrorReply, PluginError};
use wasmi::errors::{ErrorKind, MemoryError};
use wasmi::{
    Caller, Config, Engine, Error, ExternType, FuncType, Linker, Memory, Module, Store,
    StoreLimits, StoreLimitsBuilder, TrapCode, Val,
};

use crate::package::PackageError;

const WASI: &str = "wasi_snapshot_preview1";
const WASM_PAGE: u64 = 65_536;
const MAX_GUEST_OUTPUT: usize = 64 * 1024;
const MAX_TABLE_ELEMENTS: usize = 100_000;
const ERRNO_BADF: i32 = 8;
const ERRNO_FAULT: i32 = 21;
const ERRNO_NOSYS: i32 = 52;

/// A call's wall-clock budget and cancel flag, observed by every host function.
#[derive(Debug)]
pub struct CallCtl {
    cancelled: AtomicBool,
    clock: Mutex<Clock>,
}

#[derive(Debug)]
struct Clock {
    deadline: Instant,
    paused: Option<(Instant, u32)>,
}

impl CallCtl {
    pub fn new(budget: Duration) -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            clock: Mutex::new(Clock {
                deadline: Instant::now() + budget,
                paused: None,
            }),
        })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Clock> {
        self.clock.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Time left; zero when cancelled or past the deadline, `MAX` while paused.
    pub fn remaining(&self) -> Duration {
        if self.is_cancelled() {
            return Duration::ZERO;
        }
        let c = self.lock();
        if c.paused.is_some() {
            return Duration::MAX;
        }
        c.deadline.saturating_duration_since(Instant::now())
    }

    /// The error a host function returns once the budget is gone.
    ///
    /// # Errors
    /// `cancelled` or `deadline`.
    pub fn check(&self) -> Result<(), PluginError> {
        if self.is_cancelled() {
            return Err(PluginError::new(
                ErrorCode::Cancelled,
                "cancelled by the user",
            ));
        }
        if self.remaining().is_zero() {
            return Err(PluginError::new(
                ErrorCode::Deadline,
                "the call ran past its deadline",
            ));
        }
        Ok(())
    }

    /// Stops the deadline clock until the guard drops; used while a prompt is open.
    pub fn pause(self: &Arc<Self>) -> PauseGuard {
        let mut c = self.lock();
        match &mut c.paused {
            Some((_, depth)) => *depth += 1,
            None => c.paused = Some((Instant::now(), 1)),
        }
        PauseGuard(Arc::clone(self))
    }
}

#[must_use]
pub struct PauseGuard(Arc<CallCtl>);

impl Drop for PauseGuard {
    fn drop(&mut self) {
        let mut c = self.0.lock();
        if let Some((since, depth)) = &mut c.paused {
            *depth -= 1;
            if *depth == 0 {
                let spent = since.elapsed();
                c.paused = None;
                c.deadline += spent;
            }
        }
    }
}

/// What the guest's host calls reach. Replies are JSON bytes.
pub trait HostCalls: Send {
    fn set_ctl(&mut self, _ctl: Arc<CallCtl>) {}
    /// # Errors
    /// A typed error, handed to the guest as an `{"error": …}` reply.
    fn call(&mut self, name: &str, request: &[u8]) -> Result<Vec<u8>, PluginError>;
}

struct NoHost;

impl HostCalls for NoHost {
    fn call(&mut self, name: &str, _: &[u8]) -> Result<Vec<u8>, PluginError> {
        Err(PluginError::new(
            ErrorCode::PermissionDenied,
            format!("host function `{name}` is not available here"),
        ))
    }
}

struct State {
    host: Box<dyn HostCalls>,
    limits: StoreLimits,
    output: Vec<u8>,
    started: Instant,
}

/// A compiled, validated module ready to be instantiated per call.
#[derive(Clone, Debug)]
pub struct Compiled {
    module: Module,
    memory_mb: u32,
}

/// The result of one call.
#[derive(Debug)]
pub struct Outcome {
    pub result: Result<serde_json::Value, PluginError>,
    /// Guest stdout and stderr, capped.
    pub output: String,
    pub fuel_used: u64,
}

/// One engine shared by every plugin.
#[derive(Clone)]
pub struct Runtime {
    engine: Engine,
    fuel: u64,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        let mut config = Config::default();
        config.consume_fuel(true);
        Self {
            engine: Engine::new(&config),
            fuel: FUEL_PER_CALL,
        }
    }

    /// Overrides the per-call fuel budget.
    #[must_use]
    pub fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }

    /// Validates the module, checks its imports, exports and memory bounds,
    /// and confirms it reports this API major.
    ///
    /// # Errors
    /// The reason the module is refused.
    pub fn compile(&self, wasm: &[u8], memory_mb: u32) -> Result<Compiled, PackageError> {
        let refuse = |m: String| PackageError(m);
        let module = Module::new(&self.engine, wasm)
            .map_err(|e| refuse(format!("module is not valid WebAssembly: {e}")))?;

        for import in module.imports() {
            let ok = match import.module() {
                IMPORT_MODULE => import.name() == IMPORT_HOST_CALL,
                WASI => matches!(import.ty(), ExternType::Func(_)),
                _ => false,
            };
            if !ok {
                return Err(refuse(format!(
                    "module imports `{}.{}`, which Hydra does not provide",
                    import.module(),
                    import.name()
                )));
            }
        }
        for name in [EXPORT_API, EXPORT_ALLOC, EXPORT_CALL, "memory"] {
            if module.get_export(name).is_none() {
                return Err(refuse(format!("module does not export `{name}`")));
            }
        }
        let cap_pages = u64::from(memory_mb) * 1024 * 1024 / WASM_PAGE;
        let Some(ExternType::Memory(mem)) = module.get_export("memory") else {
            return Err(refuse("`memory` is not a linear memory".into()));
        };
        if mem.is_64() {
            return Err(refuse("64-bit memories are not supported".into()));
        }
        if mem.minimum() > cap_pages {
            return Err(refuse(format!(
                "initial memory exceeds memory_mb = {memory_mb}"
            )));
        }
        if mem.maximum().is_some_and(|m| m > cap_pages) {
            return Err(refuse(format!(
                "memory maximum exceeds memory_mb = {memory_mb}"
            )));
        }

        let compiled = Compiled { module, memory_mb };
        let reported = self
            .instantiate_and_api(&compiled)
            .map_err(|e| refuse(format!("module did not load: {e}")))?;
        if reported != API_MAJOR {
            return Err(refuse(format!(
                "module reports API {reported}, this Hydra speaks {API_MAJOR}"
            )));
        }
        Ok(compiled)
    }

    fn instantiate_and_api(&self, compiled: &Compiled) -> Result<i32, Error> {
        let (mut store, instance) = self.instantiate(compiled, Box::new(NoHost), None)?;
        instance.get_typed_func::<i32, i32>(&store, EXPORT_ALLOC)?;
        instance.get_typed_func::<(i32, i32, i32, i32), i64>(&store, EXPORT_CALL)?;
        let api = instance.get_typed_func::<(), i32>(&store, EXPORT_API)?;
        api.call(&mut store, ())
    }

    fn instantiate(
        &self,
        compiled: &Compiled,
        host: Box<dyn HostCalls>,
        ctl: Option<Arc<CallCtl>>,
    ) -> Result<(Store<State>, wasmi::Instance), Error> {
        let state = State {
            host,
            limits: StoreLimitsBuilder::new()
                .memory_size(compiled.memory_mb as usize * 1024 * 1024)
                .table_elements(MAX_TABLE_ELEMENTS)
                .instances(1)
                .memories(1)
                .tables(8)
                .trap_on_grow_failure(true)
                .build(),
            output: Vec::new(),
            started: Instant::now(),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_fuel(self.fuel)?;
        let mut linker = Linker::new(&self.engine);
        define_host_call(&mut linker, ctl)?;
        define_wasi(&mut linker, &compiled.module)?;
        let instance = linker.instantiate_and_start(&mut store, &compiled.module)?;
        if instance.get_export(&store, "_initialize").is_some() {
            instance
                .get_typed_func::<(), ()>(&store, "_initialize")?
                .call(&mut store, ())?;
        }
        Ok((store, instance))
    }

    /// Runs one method in a fresh instance.
    pub fn call(
        &self,
        compiled: &Compiled,
        mut host: Box<dyn HostCalls>,
        ctl: &Arc<CallCtl>,
        method: &str,
        request: &[u8],
    ) -> Outcome {
        host.set_ctl(Arc::clone(ctl));
        let mut fuel_used = 0;
        let mut store_slot: Option<Store<State>> = None;
        let result = self.run(compiled, host, ctl, method, request, &mut store_slot);
        let mut output = String::new();
        if let Some(store) = &store_slot {
            fuel_used = self.fuel.saturating_sub(store.get_fuel().unwrap_or(0));
            output = String::from_utf8_lossy(&store.data().output).into_owned();
        }
        Outcome {
            result,
            output,
            fuel_used,
        }
    }

    fn run(
        &self,
        compiled: &Compiled,
        host: Box<dyn HostCalls>,
        ctl: &Arc<CallCtl>,
        method: &str,
        request: &[u8],
        slot: &mut Option<Store<State>>,
    ) -> Result<serde_json::Value, PluginError> {
        ctl.check()?;
        if request.len() > MAX_PAYLOAD {
            return Err(PluginError::new(
                ErrorCode::InvalidInput,
                "request is too large",
            ));
        }
        let (store, instance) = self
            .instantiate(compiled, host, Some(Arc::clone(ctl)))
            .map_err(|e| classify(&e))?;
        let store = slot.insert(store);
        let memory = instance
            .get_memory(&*store, "memory")
            .ok_or_else(|| PluginError::new(ErrorCode::Trap, "module has no memory"))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&*store, EXPORT_ALLOC)
            .map_err(|e| classify(&e))?;
        let entry = instance
            .get_typed_func::<(i32, i32, i32, i32), i64>(&*store, EXPORT_CALL)
            .map_err(|e| classify(&e))?;

        let put = |store: &mut Store<State>, bytes: &[u8]| -> Result<i32, PluginError> {
            let len = i32::try_from(bytes.len())
                .map_err(|_| PluginError::new(ErrorCode::InvalidInput, "payload too large"))?;
            let ptr = alloc.call(&mut *store, len).map_err(|e| classify(&e))?;
            memory
                .write(&mut *store, ptr as u32 as usize, bytes)
                .map_err(|_| {
                    PluginError::new(ErrorCode::Trap, "guest allocator returned a bad pointer")
                })?;
            Ok(ptr)
        };
        let method_ptr = put(store, method.as_bytes())?;
        let request_ptr = put(store, request)?;
        let packed = entry
            .call(
                &mut *store,
                (
                    method_ptr,
                    method.len() as i32,
                    request_ptr,
                    request.len() as i32,
                ),
            )
            .map_err(|e| classify(&e))?;
        let (ptr, len) = unpack(packed);
        let bytes = read_guest(&memory, &*store, ptr, len)
            .map_err(|m| PluginError::new(ErrorCode::InvalidReply, m))?;
        parse_reply(&bytes)
    }
}

fn parse_reply(bytes: &[u8]) -> Result<serde_json::Value, PluginError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        PluginError::new(ErrorCode::InvalidReply, format!("reply is not JSON: {e}"))
    })?;
    if value.get("error").is_some() {
        return match serde_json::from_value::<ErrorReply>(value) {
            Ok(reply) => Err(reply.error),
            Err(e) => Err(PluginError::new(
                ErrorCode::InvalidReply,
                format!("malformed error reply: {e}"),
            )),
        };
    }
    Ok(value)
}

fn read_guest(
    memory: &Memory,
    store: &Store<State>,
    ptr: u32,
    len: u32,
) -> Result<Vec<u8>, String> {
    if len as usize > MAX_PAYLOAD {
        return Err("reply is over the payload limit".into());
    }
    let data = memory.data(store);
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .filter(|&e| e <= data.len())
        .ok_or("reply points outside guest memory")?;
    Ok(data[start..end].to_vec())
}

fn classify(e: &Error) -> PluginError {
    if matches!(
        e.kind(),
        ErrorKind::Memory(MemoryError::ResourceLimiterDeniedAllocation)
    ) || e.to_string().contains("denied allocation")
    {
        return PluginError::new(ErrorCode::OutOfMemory, "memory grew past memory_mb");
    }
    if let Some(code) = e.as_trap_code() {
        return match code {
            TrapCode::OutOfFuel => PluginError::new(
                ErrorCode::OutOfFuel,
                "the plugin used up its instruction budget",
            ),
            TrapCode::GrowthOperationLimited => {
                PluginError::new(ErrorCode::OutOfMemory, "memory grew past memory_mb")
            }
            other => PluginError::new(ErrorCode::Trap, format!("trap: {other}")),
        };
    }
    if let Some(status) = e.i32_exit_status() {
        return PluginError::new(
            ErrorCode::Trap,
            format!("plugin exited with status {status}"),
        );
    }
    PluginError::new(ErrorCode::Trap, e.to_string())
}

fn guest_memory(caller: &Caller<'_, State>) -> Result<Memory, Error> {
    caller
        .get_export("memory")
        .and_then(wasmi::Extern::into_memory)
        .ok_or_else(|| Error::new("guest has no memory"))
}

fn define_host_call(linker: &mut Linker<State>, ctl: Option<Arc<CallCtl>>) -> Result<(), Error> {
    linker.func_wrap(
        IMPORT_MODULE,
        IMPORT_HOST_CALL,
        move |mut caller: Caller<'_, State>,
              name_ptr: i32,
              name_len: i32,
              req_ptr: i32,
              req_len: i32|
              -> Result<i64, Error> {
            let memory = guest_memory(&caller)?;
            let read = |caller: &Caller<'_, State>, ptr: i32, len: i32| -> Option<Vec<u8>> {
                let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
                if len > MAX_PAYLOAD {
                    return None;
                }
                let data = memory.data(caller);
                data.get(ptr..ptr.checked_add(len)?).map(<[u8]>::to_vec)
            };
            let reply = match (
                read(&caller, name_ptr, name_len),
                read(&caller, req_ptr, req_len),
            ) {
                (Some(name), Some(req)) => match std::str::from_utf8(&name) {
                    Ok(name) => {
                        let gate = ctl.as_ref().map_or(Ok(()), |c| c.check());
                        match gate {
                            Ok(()) => caller.data_mut().host.call(name, &req),
                            Err(e) => Err(e),
                        }
                    }
                    Err(_) => Err(PluginError::new(
                        ErrorCode::InvalidInput,
                        "method is not UTF-8",
                    )),
                },
                _ => Err(PluginError::new(
                    ErrorCode::InvalidInput,
                    "host call points outside guest memory",
                )),
            };
            let bytes = match reply {
                Ok(b) if b.len() <= MAX_PAYLOAD => b,
                Ok(_) => error_bytes(&PluginError::new(
                    ErrorCode::Internal,
                    "host reply is over the payload limit",
                )),
                Err(e) => error_bytes(&e),
            };
            let alloc = caller
                .get_export(EXPORT_ALLOC)
                .and_then(wasmi::Extern::into_func)
                .ok_or_else(|| Error::new("guest has no allocator"))?
                .typed::<i32, i32>(&caller)?;
            let ptr = alloc.call(&mut caller, bytes.len() as i32)?;
            memory
                .write(&mut caller, ptr as u32 as usize, &bytes)
                .map_err(|_| Error::new("guest allocator returned a bad pointer"))?;
            Ok(pack(ptr as u32, bytes.len() as u32))
        },
    )?;
    Ok(())
}

fn error_bytes(e: &PluginError) -> Vec<u8> {
    serde_json::to_vec(&ErrorReply { error: e.clone() }).unwrap_or_default()
}

fn define_wasi(linker: &mut Linker<State>, module: &Module) -> Result<(), Error> {
    linker.func_wrap(
        WASI,
        "args_sizes_get",
        |mut c: Caller<'_, State>, argc: i32, size: i32| {
            wasi_write_u32s(&mut c, &[(argc, 0), (size, 0)])
        },
    )?;
    linker.func_wrap(WASI, "args_get", |_: Caller<'_, State>, _: i32, _: i32| {
        0i32
    })?;
    linker.func_wrap(
        WASI,
        "environ_sizes_get",
        |mut c: Caller<'_, State>, n: i32, size: i32| wasi_write_u32s(&mut c, &[(n, 0), (size, 0)]),
    )?;
    linker.func_wrap(
        WASI,
        "environ_get",
        |_: Caller<'_, State>, _: i32, _: i32| 0i32,
    )?;
    linker.func_wrap(
        WASI,
        "clock_time_get",
        |mut c: Caller<'_, State>, id: i32, _precision: i64, out: i32| {
            let nanos = match id {
                0 => SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
                1 => c.data().started.elapsed().as_nanos() + 1_000_000_000,
                _ => return ERRNO_NOSYS,
            };
            wasi_write(&mut c, out, &(nanos as u64).to_le_bytes())
        },
    )?;
    linker.func_wrap(
        WASI,
        "random_get",
        |mut c: Caller<'_, State>, ptr: i32, len: i32| {
            let Ok(memory) = guest_memory(&c) else {
                return ERRNO_FAULT;
            };
            let offset = ptr as u32 as usize;
            let len = len as u32 as usize;
            if len > MAX_PAYLOAD
                || offset
                    .checked_add(len)
                    .is_none_or(|end| end > memory.data(&c).len())
            {
                return ERRNO_FAULT;
            }
            let mut buf = vec![0u8; len];
            fill_random(&mut buf);
            wasi_write(&mut c, ptr, &buf)
        },
    )?;
    linker.func_wrap(WASI, "sched_yield", || 0i32)?;
    linker.func_wrap(WASI, "proc_exit", |code: i32| -> Result<(), Error> {
        Err(Error::i32_exit(code))
    })?;
    linker.func_wrap(
        WASI,
        "fd_write",
        |mut c: Caller<'_, State>, fd: i32, iovs: i32, n: i32, written: i32| -> i32 {
            if fd != 1 && fd != 2 {
                return ERRNO_BADF;
            }
            let Ok(mem) = guest_memory(&c) else {
                return ERRNO_FAULT;
            };
            let mut total = 0u32;
            for i in 0..n.max(0) as u32 {
                let base = iovs as u32 as usize + i as usize * 8;
                let mut head = [0u8; 8];
                if mem.read(&c, base, &mut head).is_err() {
                    return ERRNO_FAULT;
                }
                let ptr = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
                let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
                let mut chunk = vec![0u8; len.min(MAX_GUEST_OUTPUT)];
                if mem.read(&c, ptr, &mut chunk).is_err() {
                    return ERRNO_FAULT;
                }
                let out = &mut c.data_mut().output;
                let room = MAX_GUEST_OUTPUT.saturating_sub(out.len());
                out.extend_from_slice(&chunk[..chunk.len().min(room)]);
                total = total.saturating_add(len as u32);
            }
            wasi_write(&mut c, written, &total.to_le_bytes())
        },
    )?;

    const HANDLED: &[&str] = &[
        "args_sizes_get",
        "args_get",
        "environ_sizes_get",
        "environ_get",
        "clock_time_get",
        "random_get",
        "sched_yield",
        "proc_exit",
        "fd_write",
    ];
    for import in module.imports() {
        if import.module() != WASI || HANDLED.contains(&import.name()) {
            continue;
        }
        let ExternType::Func(ty) = import.ty() else {
            continue;
        };
        let errno = if import.name().starts_with("fd_prestat") || import.name().starts_with("fd_") {
            ERRNO_BADF
        } else {
            ERRNO_NOSYS
        };
        deny(linker, import.name(), ty.clone(), errno)?;
    }
    Ok(())
}

fn deny(linker: &mut Linker<State>, name: &str, ty: FuncType, errno: i32) -> Result<(), Error> {
    linker.func_new(WASI, name, ty, move |_, _, results| {
        for r in results.iter_mut() {
            if matches!(r, Val::I32(_)) {
                *r = Val::I32(errno);
            }
        }
        Ok(())
    })?;
    Ok(())
}

fn wasi_write(c: &mut Caller<'_, State>, ptr: i32, bytes: &[u8]) -> i32 {
    match guest_memory(c) {
        Ok(mem) if mem.write(&mut *c, ptr as u32 as usize, bytes).is_ok() => 0,
        _ => ERRNO_FAULT,
    }
}

fn wasi_write_u32s(c: &mut Caller<'_, State>, outs: &[(i32, u32)]) -> i32 {
    for &(ptr, value) in outs {
        let errno = wasi_write(c, ptr, &value.to_le_bytes());
        if errno != 0 {
            return errno;
        }
    }
    0
}

// RandomState keys come from the OS, which is enough for a guest's HashMap seeds.
fn fill_random(buf: &mut [u8]) {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let state = RandomState::new();
    for (i, chunk) in buf.chunks_mut(8).enumerate() {
        let mut h = state.build_hasher();
        h.write_usize(i);
        chunk.copy_from_slice(&h.finish().to_le_bytes()[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOC: &str = r#"
        (global $heap (mut i32) (i32.const 1024))
        (func (export "hydra_api") (result i32) i32.const 1)
        (func (export "hydra_alloc") (param $n i32) (result i32)
          (local $p i32)
          global.get $heap local.set $p
          global.get $heap local.get $n i32.add global.set $heap
          local.get $p)
    "#;

    fn guest(imports: &str, memory: &str, body: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module {imports}
                 (memory (export "memory") {memory})
                 (data (i32.const 400) "{{}}")
                 {ALLOC}
                 (func (export "hydra_call") (param i32 i32 i32 i32) (result i64) {body}))"#
        ))
        .unwrap()
    }

    const EMPTY_REPLY: &str = "(i64.or (i64.shl (i64.const 400) (i64.const 32)) (i64.const 2))";

    fn echo() -> Vec<u8> {
        guest(
            "",
            "2",
            "(i64.or (i64.shl (i64.extend_i32_u (local.get 2)) (i64.const 32)) (i64.extend_i32_u (local.get 3)))",
        )
    }

    fn proxy() -> Vec<u8> {
        guest(
            r#"(import "hydra" "hydra_host_call"
                (func $h (param i32 i32 i32 i32) (result i64)))"#,
            "2",
            "(call $h (local.get 0) (local.get 1) (local.get 2) (local.get 3))",
        )
    }

    struct Scripted(Arc<Mutex<Vec<String>>>);

    impl HostCalls for Scripted {
        fn call(&mut self, name: &str, request: &[u8]) -> Result<Vec<u8>, PluginError> {
            self.0.lock().unwrap().push(name.to_string());
            match name {
                "echo" => Ok(request.to_vec()),
                "deny" => Err(PluginError::new(ErrorCode::PermissionDenied, "no")),
                _ => Err(PluginError::new(ErrorCode::Internal, "unknown")),
            }
        }
    }

    fn ctl() -> Arc<CallCtl> {
        CallCtl::new(Duration::from_secs(30))
    }

    fn run(rt: &Runtime, wasm: &[u8], memory_mb: u32, method: &str, req: &str) -> Outcome {
        let compiled = rt.compile(wasm, memory_mb).unwrap();
        rt.call(&compiled, Box::new(NoHost), &ctl(), method, req.as_bytes())
    }

    #[test]
    fn a_call_round_trips_json_through_guest_memory() {
        let out = run(
            &Runtime::new(),
            &echo(),
            64,
            "resolve",
            r#"{"url":"x","n":[1,2]}"#,
        );
        assert_eq!(
            out.result.unwrap(),
            serde_json::json!({"url":"x","n":[1,2]})
        );
        assert!(out.fuel_used > 0);
    }

    #[test]
    fn an_error_reply_becomes_a_typed_error() {
        let out = run(
            &Runtime::new(),
            &echo(),
            64,
            "resolve",
            r#"{"error":{"code":"not_claimed","message":"nope"}}"#,
        );
        assert_eq!(out.result.unwrap_err().code, ErrorCode::NotClaimed);
        let odd = run(
            &Runtime::new(),
            &echo(),
            64,
            "resolve",
            r#"{"error":{"code":"made_up"}}"#,
        );
        assert_eq!(odd.result.unwrap_err().code, ErrorCode::InvalidReply);
    }

    #[test]
    fn a_non_json_reply_is_invalid() {
        let out = run(&Runtime::new(), &echo(), 64, "resolve", "not json");
        assert_eq!(out.result.unwrap_err().code, ErrorCode::InvalidReply);
    }

    #[test]
    fn a_reply_pointing_outside_memory_is_invalid() {
        let wasm = guest("", "1", "(i64.const -1)");
        let out = run(&Runtime::new(), &wasm, 64, "resolve", "{}");
        assert_eq!(out.result.unwrap_err().code, ErrorCode::InvalidReply);
    }

    #[test]
    fn host_calls_reach_the_host_and_errors_reach_the_guest() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let rt = Runtime::new();
        let compiled = rt.compile(&proxy(), 64).unwrap();
        let ok = rt.call(
            &compiled,
            Box::new(Scripted(log.clone())),
            &ctl(),
            "echo",
            br#"{"a":1}"#,
        );
        assert_eq!(ok.result.unwrap(), serde_json::json!({"a":1}));
        let denied = rt.call(
            &compiled,
            Box::new(Scripted(log.clone())),
            &ctl(),
            "deny",
            b"{}",
        );
        assert_eq!(denied.result.unwrap_err().code, ErrorCode::PermissionDenied);
        assert_eq!(*log.lock().unwrap(), ["echo", "deny"]);
    }

    #[test]
    fn an_infinite_loop_runs_out_of_fuel_and_the_next_call_is_clean() {
        let spin = guest("", "2", "(loop $l (br $l)) (i64.const 0)");
        let rt = Runtime::new().with_fuel(100_000);
        let out = run(&rt, &spin, 64, "resolve", "{}");
        assert_eq!(out.result.unwrap_err().code, ErrorCode::OutOfFuel);

        let compiled = rt.compile(&echo(), 64).unwrap();
        let good = rt.call(&compiled, Box::new(NoHost), &ctl(), "resolve", b"{}");
        assert!(good.result.is_ok());
    }

    #[test]
    fn growing_past_memory_mb_is_out_of_memory() {
        let hog = guest(
            "",
            "1",
            &format!("(drop (memory.grow (i32.const 1000))) {EMPTY_REPLY}"),
        );
        let rt = Runtime::new();
        let out = run(&rt, &hog, 1, "resolve", "{}");
        let err = out.result.unwrap_err();
        assert_eq!(err.code, ErrorCode::OutOfMemory, "{}", err.message);

        let compiled = rt.compile(&echo(), 1).unwrap();
        assert!(rt
            .call(&compiled, Box::new(NoHost), &ctl(), "resolve", b"{}")
            .result
            .is_ok());
    }

    #[test]
    fn growing_within_the_cap_is_fine() {
        let grow = guest(
            "",
            "1",
            &format!("(drop (memory.grow (i32.const 4))) {EMPTY_REPLY}"),
        );
        assert!(run(&Runtime::new(), &grow, 1, "resolve", "{}")
            .result
            .is_ok());
    }

    #[test]
    fn a_trap_is_a_trap() {
        let wasm = guest("", "1", "unreachable");
        let out = run(&Runtime::new(), &wasm, 64, "resolve", "{}");
        assert_eq!(out.result.unwrap_err().code, ErrorCode::Trap);
    }

    #[test]
    fn modules_are_vetted_at_compile_time() {
        let rt = Runtime::new();
        let bad_import = guest(
            r#"(import "env" "system" (func (param i32) (result i32)))"#,
            "1",
            "(i64.const 0)",
        );
        assert!(rt
            .compile(&bad_import, 64)
            .unwrap_err()
            .0
            .contains("env.system"));

        let other_hydra = guest(r#"(import "hydra" "other" (func))"#, "1", "(i64.const 0)");
        assert!(rt.compile(&other_hydra, 64).is_err());

        assert!(rt.compile(b"\0asm\x01\0\0\0junk", 64).is_err());

        let no_exports = wat::parse_str("(module (memory (export \"memory\") 1))").unwrap();
        assert!(rt
            .compile(&no_exports, 64)
            .unwrap_err()
            .0
            .contains("hydra_api"));

        let wrong_api = wat::parse_str(
            r#"(module (memory (export "memory") 1)
                 (func (export "hydra_api") (result i32) i32.const 2)
                 (func (export "hydra_alloc") (param i32) (result i32) i32.const 0)
                 (func (export "hydra_call") (param i32 i32 i32 i32) (result i64) i64.const 0))"#,
        )
        .unwrap();
        assert!(rt.compile(&wrong_api, 64).unwrap_err().0.contains("API 2"));
    }

    #[test]
    fn memory_bounds_must_fit_memory_mb() {
        let rt = Runtime::new();
        let big_max = guest("", "1 20000", "(i64.const 0)");
        assert!(rt.compile(&big_max, 1).unwrap_err().0.contains("maximum"));
        let big_min = guest("", "40", "(i64.const 0)");
        assert!(rt.compile(&big_min, 1).unwrap_err().0.contains("initial"));
        assert!(rt.compile(&guest("", "1 16", "(i64.const 0)"), 1).is_ok());
    }

    #[test]
    fn guest_stdout_is_captured_not_parsed() {
        let wasm = guest(
            r#"(import "wasi_snapshot_preview1" "fd_write"
                (func $w (param i32 i32 i32 i32) (result i32)))
               (data (i32.const 100) "hello\n")"#,
            "2",
            &format!(
                r#"(i32.store (i32.const 200) (i32.const 100))
                   (i32.store (i32.const 204) (i32.const 6))
                   (if (call $w (i32.const 1) (i32.const 200) (i32.const 1) (i32.const 300))
                       (then (return (i64.const -1))))
                   {EMPTY_REPLY}"#
            ),
        );
        let out = run(&Runtime::new(), &wasm, 64, "resolve", "{}");
        assert!(out.result.is_ok());
        assert_eq!(out.output, "hello\n");
    }

    #[test]
    fn wasi_denies_everything_but_the_basics() {
        let wasm = guest(
            r#"(import "wasi_snapshot_preview1" "path_open"
                 (func $po (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
               (import "wasi_snapshot_preview1" "fd_prestat_get"
                 (func $ps (param i32 i32) (result i32)))
               (import "wasi_snapshot_preview1" "fd_write"
                 (func $w (param i32 i32 i32 i32) (result i32)))
               (import "wasi_snapshot_preview1" "random_get"
                 (func $r (param i32 i32) (result i32)))
               (import "wasi_snapshot_preview1" "clock_time_get"
                 (func $c (param i32 i64 i32) (result i32)))
               (import "wasi_snapshot_preview1" "args_sizes_get"
                 (func $a (param i32 i32) (result i32)))
               (import "wasi_snapshot_preview1" "environ_sizes_get"
                 (func $e (param i32 i32) (result i32)))"#,
            "2",
            &format!(
                r#"(if (i32.ne (call $po (i32.const 3) (i32.const 0) (i32.const 0) (i32.const 0)
                        (i32.const 0) (i64.const 0) (i64.const 0) (i32.const 0) (i32.const 0))
                      (i32.const 52)) (then (return (i64.const -1))))
                   (if (i32.ne (call $ps (i32.const 3) (i32.const 0)) (i32.const 8))
                       (then (return (i64.const -1))))
                   (if (i32.ne (call $w (i32.const 7) (i32.const 0) (i32.const 0) (i32.const 0))
                       (i32.const 8)) (then (return (i64.const -1))))
                   (if (call $r (i32.const 600) (i32.const 16)) (then (return (i64.const -1))))
                   (if (call $c (i32.const 0) (i64.const 1) (i32.const 700)) (then (return (i64.const -1))))
                   (if (call $c (i32.const 1) (i64.const 1) (i32.const 700)) (then (return (i64.const -1))))
                   (if (i32.ne (call $c (i32.const 9) (i64.const 1) (i32.const 700)) (i32.const 52))
                       (then (return (i64.const -1))))
                   (if (call $a (i32.const 800) (i32.const 804)) (then (return (i64.const -1))))
                   (if (call $e (i32.const 800) (i32.const 804)) (then (return (i64.const -1))))
                   (if (i32.load (i32.const 800)) (then (return (i64.const -1))))
                   {EMPTY_REPLY}"#
            ),
        );
        assert!(run(&Runtime::new(), &wasm, 64, "resolve", "{}")
            .result
            .is_ok());
    }

    #[test]
    fn proc_exit_is_a_trap() {
        let wasm = guest(
            r#"(import "wasi_snapshot_preview1" "proc_exit" (func $x (param i32)))"#,
            "1",
            "(call $x (i32.const 3)) (i64.const 0)",
        );
        let err = run(&Runtime::new(), &wasm, 64, "resolve", "{}")
            .result
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Trap);
        assert!(err.message.contains("status 3"), "{}", err.message);
    }

    #[test]
    fn a_cancelled_call_does_not_start_and_cancels_host_calls() {
        let rt = Runtime::new();
        let compiled = rt.compile(&proxy(), 64).unwrap();
        let c = ctl();
        c.cancel();
        let out = rt.call(&compiled, Box::new(NoHost), &c, "echo", b"{}");
        assert_eq!(out.result.unwrap_err().code, ErrorCode::Cancelled);

        struct CancelOnFirst(Arc<CallCtl>);
        impl HostCalls for CancelOnFirst {
            fn call(&mut self, _: &str, req: &[u8]) -> Result<Vec<u8>, PluginError> {
                self.0.cancel();
                Ok(req.to_vec())
            }
        }
        let guest2 = guest(
            r#"(import "hydra" "hydra_host_call"
                (func $h (param i32 i32 i32 i32) (result i64)))"#,
            "2",
            "(drop (call $h (local.get 0) (local.get 1) (local.get 2) (local.get 3)))
             (call $h (local.get 0) (local.get 1) (local.get 2) (local.get 3))",
        );
        let compiled = rt.compile(&guest2, 64).unwrap();
        let c = ctl();
        let out = rt.call(
            &compiled,
            Box::new(CancelOnFirst(c.clone())),
            &c,
            "x",
            b"{}",
        );
        assert_eq!(out.result.unwrap_err().code, ErrorCode::Cancelled);
    }

    #[test]
    fn the_deadline_reaches_host_calls_and_pauses_for_prompts() {
        let c = CallCtl::new(Duration::from_millis(60));
        assert!(c.check().is_ok());
        {
            let _guard = c.pause();
            std::thread::sleep(Duration::from_millis(120));
            assert!(c.check().is_ok());
            assert_eq!(c.remaining(), Duration::MAX);
        }
        assert!(c.check().is_ok(), "pause time is not charged");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(c.check().unwrap_err().code, ErrorCode::Deadline);

        let nested = CallCtl::new(Duration::from_secs(1));
        let a = nested.pause();
        let b = nested.pause();
        drop(a);
        assert_eq!(nested.remaining(), Duration::MAX);
        drop(b);
        assert!(nested.remaining() < Duration::MAX);
    }

    #[test]
    fn oversized_host_replies_become_internal_errors() {
        struct Huge;
        impl HostCalls for Huge {
            fn call(&mut self, _: &str, _: &[u8]) -> Result<Vec<u8>, PluginError> {
                Ok(vec![b' '; MAX_PAYLOAD + 1])
            }
        }
        let rt = Runtime::new();
        let compiled = rt.compile(&proxy(), 256).unwrap();
        let out = rt.call(&compiled, Box::new(Huge), &ctl(), "x", b"{}");
        assert_eq!(out.result.unwrap_err().code, ErrorCode::Internal);
    }
}
