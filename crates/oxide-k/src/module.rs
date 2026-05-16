//! # Module Orchestration
//!
//! Defines the traits and the registry that the kernel uses to manage the
//! lifecycle of native Rust modules and WebAssembly (WASM) plugins.
//!
//! The micro-kernel does not bake in a single module ABI. Instead it exposes
//! two complementary traits:
//!
//! * [`Module`] – implemented by native, in-process Rust modules. Used for the
//!   Layer 1 "Native Core" components in the Rust Oxide architecture (network
//!   stack, SQLite engine, etc.).
//! * [`WasmModule`] – implemented by WebAssembly plugins. Used for the Layer 2
//!   sandboxed components (extractors, parsers, `oxide-compress`, etc.).
//!
//! The [`ModuleManager`] keeps a map of loaded modules keyed by id and drives
//! them through a small state machine (`Loaded -> Starting -> Running ->
//! Stopping -> Stopped`).

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::bus::MessageBus;
use crate::error::{KernelError, Result};

/// The lifecycle state of a module known to the [`ModuleManager`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleState {
    /// The module has been registered but not yet started.
    Loaded,
    /// The module is transitioning from `Loaded` to `Running`.
    Starting,
    /// The module is fully started and accepting work.
    Running,
    /// The module is transitioning from `Running` to `Stopped`.
    Stopping,
    /// The module has been stopped.
    Stopped,
    /// The module crashed or its `start`/`stop` hook returned an error.
    Failed,
}

/// Static information about a module supplied at registration time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleMetadata {
    /// Stable, unique module id (e.g. `"oxide-mirror"`).
    pub id: String,
    /// Human-readable module name.
    pub name: String,
    /// Semantic version string.
    pub version: String,
    /// Module kind / execution layer.
    pub kind: ModuleKind,
    /// Free-form description.
    pub description: Option<String>,
}

/// The execution layer / runtime that hosts a given module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleKind {
    /// A native Rust module ([`Module`]). Layer 1 in the architecture.
    Native,
    /// A WebAssembly plugin ([`WasmModule`]). Layer 2 in the architecture.
    Wasm,
}

/// Trait implemented by all native, in-process modules.
///
/// Methods are `async` because most kernel work flows through Tokio and many
/// modules need to await I/O during `start`/`stop`. Implementors should not
/// block the runtime in these hooks.
#[async_trait]
pub trait Module: Send + Sync + 'static {
    /// Static module metadata.
    fn metadata(&self) -> ModuleMetadata;

    /// Initialize the module. Called once, before `start`.
    ///
    /// The `bus` is provided so the module can subscribe to events or publish
    /// initial state. The default implementation is a no-op.
    async fn init(&mut self, _bus: MessageBus) -> Result<()> {
        Ok(())
    }

    /// Start the module. Must be idempotent.
    async fn start(&mut self) -> Result<()>;

    /// Stop the module. Must be idempotent and quick; long-running cleanup
    /// belongs in a separate task signalled from here.
    async fn stop(&mut self) -> Result<()>;
}

/// Trait implemented by WebAssembly plugins.
///
/// Right now the trait is a thin placeholder: it mirrors [`Module`] but uses a
/// distinct identity so the manager can keep native and WASM modules separate
/// and apply different sandboxing/security policies later (e.g. wasmtime
/// engine configuration, capability gating).
#[async_trait]
pub trait WasmModule: Send + Sync + 'static {
    /// Static module metadata. The `kind` field should be
    /// [`ModuleKind::Wasm`].
    fn metadata(&self) -> ModuleMetadata;

    /// Instantiate the WASM module inside the host runtime.
    async fn instantiate(&mut self, _bus: MessageBus) -> Result<()> {
        Ok(())
    }

    /// Start the WASM module's main entry point.
    async fn start(&mut self) -> Result<()>;

    /// Stop and tear down the WASM instance.
    async fn stop(&mut self) -> Result<()>;
}

/// Internal enum that lets the manager hold both module flavours in a single map.
enum AnyModule {
    Native(Box<dyn Module>),
    Wasm(Box<dyn WasmModule>),
}

impl AnyModule {
    fn metadata(&self) -> ModuleMetadata {
        match self {
            AnyModule::Native(m) => m.metadata(),
            AnyModule::Wasm(m) => m.metadata(),
        }
    }

    async fn init(&mut self, bus: MessageBus) -> Result<()> {
        match self {
            AnyModule::Native(m) => m.init(bus).await,
            AnyModule::Wasm(m) => m.instantiate(bus).await,
        }
    }

    async fn start(&mut self) -> Result<()> {
        match self {
            AnyModule::Native(m) => m.start().await,
            AnyModule::Wasm(m) => m.start().await,
        }
    }

    async fn stop(&mut self) -> Result<()> {
        match self {
            AnyModule::Native(m) => m.stop().await,
            AnyModule::Wasm(m) => m.stop().await,
        }
    }
}

struct ModuleEntry {
    module: AnyModule,
    state: ModuleState,
}

/// Manages the set of modules loaded into the kernel.
///
/// `ModuleManager` is cheaply cloneable (it wraps its state in an `Arc`) so it
/// can be handed to subsystems that need to inspect module status, e.g. the
/// future CLI.
#[derive(Clone)]
pub struct ModuleManager {
    bus: MessageBus,
    inner: Arc<RwLock<HashMap<String, ModuleEntry>>>,
}

impl ModuleManager {
    /// Construct a new manager bound to the given message bus.
    pub fn new(bus: MessageBus) -> Self {
        Self {
            bus,
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a native Rust [`Module`] with the manager.
    ///
    /// The module is moved into the manager and held until it is unloaded.
    pub async fn register_native<M: Module>(&self, module: M) -> Result<()> {
        let metadata = module.metadata();
        self.insert(metadata.id.clone(), AnyModule::Native(Box::new(module)))
            .await
    }

    /// Register a [`WasmModule`] plugin with the manager.
    pub async fn register_wasm<M: WasmModule>(&self, module: M) -> Result<()> {
        let metadata = module.metadata();
        self.insert(metadata.id.clone(), AnyModule::Wasm(Box::new(module)))
            .await
    }

    async fn insert(&self, id: String, module: AnyModule) -> Result<()> {
        let mut map = self.inner.write().await;
        if map.contains_key(&id) {
            return Err(KernelError::DuplicateModule(id));
        }
        map.insert(
            id,
            ModuleEntry {
                module,
                state: ModuleState::Loaded,
            },
        );
        Ok(())
    }

    /// Drive a registered module's `init` hook.
    pub async fn init(&self, id: &str) -> Result<()> {
        let mut map = self.inner.write().await;
        let entry = map
            .get_mut(id)
            .ok_or_else(|| KernelError::UnknownModule(id.to_string()))?;
        entry.module.init(self.bus.clone()).await
    }

    /// Start a registered module.
    pub async fn start(&self, id: &str) -> Result<()> {
        let mut map = self.inner.write().await;
        let entry = map
            .get_mut(id)
            .ok_or_else(|| KernelError::UnknownModule(id.to_string()))?;

        entry.state = ModuleState::Starting;
        match entry.module.start().await {
            Ok(()) => {
                entry.state = ModuleState::Running;
                let id_owned = id.to_string();
                drop(map);
                self.bus
                    .emit_event(
                        "kernel",
                        crate::bus::Event::ModuleStarted {
                            module_id: id_owned,
                        },
                    )
                    .await?;
                Ok(())
            }
            Err(e) => {
                entry.state = ModuleState::Failed;
                Err(e)
            }
        }
    }

    /// Stop a registered module.
    pub async fn stop(&self, id: &str) -> Result<()> {
        let mut map = self.inner.write().await;
        let entry = map
            .get_mut(id)
            .ok_or_else(|| KernelError::UnknownModule(id.to_string()))?;

        entry.state = ModuleState::Stopping;
        match entry.module.stop().await {
            Ok(()) => {
                entry.state = ModuleState::Stopped;
                let id_owned = id.to_string();
                drop(map);
                self.bus
                    .emit_event(
                        "kernel",
                        crate::bus::Event::ModuleStopped {
                            module_id: id_owned,
                        },
                    )
                    .await?;
                Ok(())
            }
            Err(e) => {
                entry.state = ModuleState::Failed;
                Err(e)
            }
        }
    }

    /// Stop and remove a module from the manager entirely.
    pub async fn unload(&self, id: &str) -> Result<()> {
        // Best-effort stop first; ignore the error if the module is already
        // stopped — the goal is to remove it.
        let _ = self.stop(id).await;
        let mut map = self.inner.write().await;
        map.remove(id)
            .ok_or_else(|| KernelError::UnknownModule(id.to_string()))?;
        Ok(())
    }

    /// Return the current state of a module, if it is registered.
    pub async fn state(&self, id: &str) -> Option<ModuleState> {
        self.inner.read().await.get(id).map(|e| e.state)
    }

    /// Return a snapshot of all registered modules and their states.
    pub async fn list(&self) -> Vec<(ModuleMetadata, ModuleState)> {
        self.inner
            .read()
            .await
            .values()
            .map(|e| (e.module.metadata(), e.state))
            .collect()
    }
}

impl Debug for ModuleManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleManager").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Minimal native module used to exercise the manager.
    struct EchoModule {
        meta: ModuleMetadata,
        start_count: Arc<AtomicUsize>,
        stop_count: Arc<AtomicUsize>,
    }

    impl EchoModule {
        fn new(id: &str) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
            let start = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    meta: ModuleMetadata {
                        id: id.to_string(),
                        name: format!("Echo {id}"),
                        version: "0.1.0".into(),
                        kind: ModuleKind::Native,
                        description: None,
                    },
                    start_count: start.clone(),
                    stop_count: stop.clone(),
                },
                start,
                stop,
            )
        }
    }

    #[async_trait]
    impl Module for EchoModule {
        fn metadata(&self) -> ModuleMetadata {
            self.meta.clone()
        }
        async fn start(&mut self) -> Result<()> {
            self.start_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn stop(&mut self) -> Result<()> {
            self.stop_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Minimal WASM module stub.
    struct StubWasm {
        meta: ModuleMetadata,
    }

    #[async_trait]
    impl WasmModule for StubWasm {
        fn metadata(&self) -> ModuleMetadata {
            self.meta.clone()
        }
        async fn start(&mut self) -> Result<()> {
            Ok(())
        }
        async fn stop(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn register_start_stop_native_module() {
        let bus = MessageBus::new();
        let mgr = ModuleManager::new(bus);
        let (module, started, stopped) = EchoModule::new("echo");

        mgr.register_native(module).await.unwrap();
        assert_eq!(mgr.state("echo").await, Some(ModuleState::Loaded));

        mgr.start("echo").await.unwrap();
        assert_eq!(mgr.state("echo").await, Some(ModuleState::Running));
        assert_eq!(started.load(Ordering::SeqCst), 1);

        mgr.stop("echo").await.unwrap();
        assert_eq!(mgr.state("echo").await, Some(ModuleState::Stopped));
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn duplicate_registration_is_rejected() {
        let mgr = ModuleManager::new(MessageBus::new());
        let (m1, _, _) = EchoModule::new("dup");
        let (m2, _, _) = EchoModule::new("dup");

        mgr.register_native(m1).await.unwrap();
        let err = mgr.register_native(m2).await.unwrap_err();
        assert!(matches!(err, KernelError::DuplicateModule(_)));
    }

    #[tokio::test]
    async fn unknown_module_returns_error() {
        let mgr = ModuleManager::new(MessageBus::new());
        let err = mgr.start("missing").await.unwrap_err();
        assert!(matches!(err, KernelError::UnknownModule(_)));
    }

    #[tokio::test]
    async fn register_wasm_module() {
        let mgr = ModuleManager::new(MessageBus::new());
        let stub = StubWasm {
            meta: ModuleMetadata {
                id: "wasm-stub".into(),
                name: "Stub".into(),
                version: "0.0.1".into(),
                kind: ModuleKind::Wasm,
                description: None,
            },
        };

        mgr.register_wasm(stub).await.unwrap();
        mgr.start("wasm-stub").await.unwrap();
        assert_eq!(mgr.state("wasm-stub").await, Some(ModuleState::Running));

        let list = mgr.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0.kind, ModuleKind::Wasm);
    }

    #[tokio::test]
    async fn unload_removes_module() {
        let mgr = ModuleManager::new(MessageBus::new());
        let (m, _, _) = EchoModule::new("echo");
        mgr.register_native(m).await.unwrap();
        mgr.start("echo").await.unwrap();

        mgr.unload("echo").await.unwrap();
        assert!(mgr.state("echo").await.is_none());
    }

    #[tokio::test]
    async fn start_emits_module_started_event() {
        let bus = MessageBus::new();
        let mut sub = bus.subscribe().await;
        let mgr = ModuleManager::new(bus);

        let (m, _, _) = EchoModule::new("echo");
        mgr.register_native(m).await.unwrap();
        mgr.start("echo").await.unwrap();

        let env = sub.receiver.recv().await.unwrap();
        match env.message {
            crate::bus::Message::Event(crate::bus::Event::ModuleStarted { module_id }) => {
                assert_eq!(module_id, "echo");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
