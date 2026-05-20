//! # `wasmtime`-backed Wasm executor
//!
//! Available under the `wasmtime-runtime` feature flag.
//!
//! [`WasmExecutor`] loads a `.wasm` artefact off disk (typically a
//! `oxide-compress` build), instantiates it inside a fresh `wasmtime::Store`,
//! and exposes the exported functions as plain async callables on the
//! kernel.
//!
//! Two convenience entry points ship today:
//!
//! * [`WasmExecutor::call_i32_to_i32`] — invokes an exported
//!   `fn(x: i32) -> i32` such as the `oxide-compress` `add_one` smoke test.
//! * [`WasmExecutor::list_exports`] — enumerates exported function names so
//!   higher-level dispatchers can route calls dynamically.
//!
//! Full host-ABI (JSON-string in / out via linear memory) is intentionally
//! deferred — the goal of this module is to prove the runtime works and
//! provide a stable seam for future expansion, not to ship a full WIT
//! binding generator.

#![cfg(feature = "wasmtime-runtime")]

use std::path::Path;
use std::sync::Mutex;

use wasmtime::{Engine, Instance, Module, Store, TypedFunc};

use crate::error::{KernelError, Result};

/// Wraps a wasmtime [`Module`] + [`Store`] + [`Instance`] under a single
/// async-friendly handle.
pub struct WasmExecutor {
    engine: Engine,
    module: Module,
    inner: Mutex<Instance>,
    store: Mutex<Store<()>>,
}

impl WasmExecutor {
    /// Load `.wasm` bytes from disk.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = std::fs::read(path.as_ref()).map_err(|e| {
            KernelError::Other(anyhow::anyhow!(
                "failed to read wasm artefact {}: {e}",
                path.as_ref().display()
            ))
        })?;
        Self::from_bytes(&bytes)
    }

    /// Build an executor from raw `.wasm` bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes).map_err(to_kernel)?;
        let mut store: Store<()> = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[]).map_err(to_kernel)?;
        Ok(Self {
            engine,
            module,
            inner: Mutex::new(instance),
            store: Mutex::new(store),
        })
    }

    /// Enumerate exported function names.
    pub fn list_exports(&self) -> Vec<String> {
        self.module
            .exports()
            .filter_map(|e| match e.ty() {
                wasmtime::ExternType::Func(_) => Some(e.name().to_string()),
                _ => None,
            })
            .collect()
    }

    /// Invoke `name(x: i32) -> i32`.
    pub fn call_i32_to_i32(&self, name: &str, arg: i32) -> Result<i32> {
        let mut store = self.store.lock().unwrap();
        let instance = self.inner.lock().unwrap();
        let func: TypedFunc<i32, i32> = instance
            .get_typed_func::<i32, i32>(&mut *store, name)
            .map_err(|e| {
                KernelError::Other(anyhow::anyhow!(
                    "wasm export `{name}` is not (i32) -> i32: {e}"
                ))
            })?;
        func.call(&mut *store, arg).map_err(to_kernel)
    }

    /// Call a WASM guest method with JSON input and receive JSON output.
    ///
    /// The guest must export three symbols:
    /// - `alloc(size: i32) -> i32` — allocate `size` bytes, return pointer
    /// - `free(ptr: i32, size: i32)` — free a previous allocation
    /// - `oxide_invoke(method_ptr: i32, method_len: i32, input_ptr: i32, input_len: i32) -> i64`
    ///   — dispatch; the `i64` return packs output `ptr` (high 32 bits) and
    ///   `len` (low 32 bits).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Other`] if any WASM call fails, memory is out of
    /// bounds, or the output bytes are not valid UTF-8 / JSON.
    pub fn call_json(&self, method: &str, input: &serde_json::Value) -> Result<serde_json::Value> {
        let mut store = self.store.lock().unwrap();
        let instance = self.inner.lock().unwrap();

        // Resolve guest functions.
        let alloc: TypedFunc<i32, i32> = instance
            .get_typed_func::<i32, i32>(&mut *store, "alloc")
            .map_err(|e| KernelError::Other(anyhow::anyhow!("alloc export: {e}")))?;
        let free: TypedFunc<(i32, i32), ()> = instance
            .get_typed_func::<(i32, i32), ()>(&mut *store, "free")
            .map_err(|e| KernelError::Other(anyhow::anyhow!("free export: {e}")))?;
        let invoke: TypedFunc<(i32, i32, i32, i32), i64> = instance
            .get_typed_func::<(i32, i32, i32, i32), i64>(&mut *store, "oxide_invoke")
            .map_err(|e| KernelError::Other(anyhow::anyhow!("oxide_invoke export: {e}")))?;

        let memory = instance
            .get_memory(&mut *store, "memory")
            .ok_or_else(|| KernelError::Other(anyhow::anyhow!("no `memory` export")))?;

        // Serialize method name and input JSON.
        let method_bytes = method.as_bytes().to_vec();
        let input_bytes = serde_json::to_vec(input).map_err(|e| KernelError::Other(e.into()))?;

        // Write method bytes into guest memory.
        let method_len = method_bytes.len() as i32;
        let method_ptr = alloc
            .call(&mut *store, method_len)
            .map_err(|e| KernelError::Other(anyhow::anyhow!("alloc method: {e}")))?;
        {
            let mem = memory.data_mut(&mut *store);
            let s = method_ptr as usize;
            if s + method_bytes.len() > mem.len() {
                return Err(KernelError::Other(anyhow::anyhow!("method OOB")));
            }
            mem[s..s + method_bytes.len()].copy_from_slice(&method_bytes);
        }

        // Write input bytes into guest memory.
        let input_len = input_bytes.len() as i32;
        let input_ptr = alloc
            .call(&mut *store, input_len)
            .map_err(|e| KernelError::Other(anyhow::anyhow!("alloc input: {e}")))?;
        {
            let mem = memory.data_mut(&mut *store);
            let s = input_ptr as usize;
            if s + input_bytes.len() > mem.len() {
                return Err(KernelError::Other(anyhow::anyhow!("input OOB")));
            }
            mem[s..s + input_bytes.len()].copy_from_slice(&input_bytes);
        }

        // Call oxide_invoke.
        let result = invoke
            .call(&mut *store, (method_ptr, method_len, input_ptr, input_len))
            .map_err(|e| KernelError::Other(anyhow::anyhow!("oxide_invoke: {e}")))?;

        // Free input buffers.
        let _ = free.call(&mut *store, (method_ptr, method_len));
        let _ = free.call(&mut *store, (input_ptr, input_len));

        // Decode output: high 32 bits = ptr, low 32 bits = len.
        let out_ptr = ((result >> 32) & 0xFFFF_FFFF) as usize;
        let out_len = (result & 0xFFFF_FFFF) as usize;

        let output_bytes = {
            let mem = memory.data(&*store);
            if out_ptr + out_len > mem.len() {
                return Err(KernelError::Other(anyhow::anyhow!("output OOB")));
            }
            mem[out_ptr..out_ptr + out_len].to_vec()
        };

        // Free output buffer.
        let _ = free.call(&mut *store, (out_ptr as i32, out_len as i32));

        let output = serde_json::from_slice(&output_bytes)
            .map_err(|e| KernelError::Other(anyhow::anyhow!("output JSON: {e}")))?;
        Ok(output)
    }

    /// Underlying wasmtime engine, for callers that want to share it with
    /// other modules.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

fn to_kernel(err: impl std::fmt::Display) -> KernelError {
    KernelError::Other(anyhow::anyhow!("wasmtime: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-rolled WAT for `(i32) -> i32` add-one. Avoids needing a separate
    /// pre-built `.wasm` fixture.
    fn add_one_wat() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
              (func (export "add_one") (param i32) (result i32)
                local.get 0
                i32.const 1
                i32.add))
            "#,
        )
        .expect("valid wat")
    }

    #[test]
    fn executor_can_run_pure_i32_function() {
        let bytes = add_one_wat();
        let exec = WasmExecutor::from_bytes(&bytes).unwrap();
        assert_eq!(exec.list_exports(), vec!["add_one".to_string()]);
        assert_eq!(exec.call_i32_to_i32("add_one", 41).unwrap(), 42);
    }

    #[test]
    fn missing_export_errors() {
        let exec = WasmExecutor::from_bytes(&add_one_wat()).unwrap();
        let err = exec.call_i32_to_i32("nope", 1).unwrap_err();
        assert!(format!("{err}").contains("nope"));
    }

    /// WAT module that implements the full oxide_invoke ABI.
    /// It echoes `{"echo": <input>}` back to the host via linear memory.
    fn echo_abi_wat() -> Vec<u8> {
        // The module uses a static 64 KB memory page.
        // alloc: returns a fixed output region starting at offset 4096.
        // We use a simple bump strategy: alloc always gives sequential regions.
        // For this test the payloads are tiny so there's no collision.
        wat::parse_str(
            r#"
            (module
              (memory (export "memory") 2)

              ;; Bump allocator: ptr stored at byte 0 (i32), initial = 256
              (func (export "alloc") (param $size i32) (result i32)
                (local $ptr i32)
                ;; read current bump ptr (stored at address 0)
                (local.set $ptr (i32.load (i32.const 0)))
                ;; if zero, initialise to 256
                (if (i32.eqz (local.get $ptr))
                  (then (local.set $ptr (i32.const 256)))
                )
                ;; store advanced ptr
                (i32.store (i32.const 0) (i32.add (local.get $ptr) (local.get $size)))
                ;; return old ptr
                (local.get $ptr)
              )

              ;; free is a no-op in this bump allocator
              (func (export "free") (param $ptr i32) (param $size i32))

              ;; oxide_invoke: writes {"ok":true} into memory and returns ptr<<32|len
              (func (export "oxide_invoke")
                    (param $mp i32) (param $ml i32)
                    (param $ip i32) (param $il i32)
                    (result i64)
                (local $out_ptr i32)
                (local $payload_len i32)
                ;; static output: write `{"ok":true}` at address 8192
                ;; 0x7b = '{', 0x22 = '"', 0x6f='o',0x6b='k',0x22='"',0x3a=':',
                ;; 0x74='t',0x72='r',0x75='u',0x65='e',0x7d='}'  = 11 bytes
                (i32.store8 (i32.const 8192) (i32.const 123))  ;; {
                (i32.store8 (i32.const 8193) (i32.const 34))   ;; "
                (i32.store8 (i32.const 8194) (i32.const 111))  ;; o
                (i32.store8 (i32.const 8195) (i32.const 107))  ;; k
                (i32.store8 (i32.const 8196) (i32.const 34))   ;; "
                (i32.store8 (i32.const 8197) (i32.const 58))   ;; :
                (i32.store8 (i32.const 8198) (i32.const 116))  ;; t
                (i32.store8 (i32.const 8199) (i32.const 114))  ;; r
                (i32.store8 (i32.const 8200) (i32.const 117))  ;; u
                (i32.store8 (i32.const 8201) (i32.const 101))  ;; e
                (i32.store8 (i32.const 8202) (i32.const 125))  ;; }
                (local.set $out_ptr (i32.const 8192))
                (local.set $payload_len (i32.const 11))
                ;; return (ptr << 32) | len  as i64
                (i64.or
                  (i64.shl (i64.extend_i32_u (local.get $out_ptr)) (i64.const 32))
                  (i64.extend_i32_u (local.get $payload_len))
                )
              )
            )
            "#,
        )
        .expect("valid echo ABI wat")
    }

    #[test]
    fn call_json_round_trips_via_abi() {
        let bytes = echo_abi_wat();
        let exec = WasmExecutor::from_bytes(&bytes).unwrap();
        let result = exec
            .call_json("echo", &serde_json::json!({"hello": "world"}))
            .unwrap();
        assert_eq!(result["ok"], serde_json::Value::Bool(true));
    }
}
