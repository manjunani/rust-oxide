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
}
