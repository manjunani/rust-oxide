//! The [`Kernel`] façade ties the three subsystems together.

use crate::bus::MessageBus;
use crate::error::Result;
use crate::module::ModuleManager;
use crate::registry::StateRegistry;

/// The Oxide micro-kernel.
///
/// Owns the [`MessageBus`], [`ModuleManager`] and [`StateRegistry`] and exposes
/// them as cheap-to-clone handles. Cloning the kernel itself is also cheap and
/// produces a handle pointing at the same underlying state.
#[derive(Clone, Debug)]
pub struct Kernel {
    bus: MessageBus,
    modules: ModuleManager,
    registry: StateRegistry,
}

impl Kernel {
    /// Build a kernel backed by an in-memory state registry. Intended for
    /// development, testing, and the bootstrap CLI.
    pub async fn in_memory() -> Result<Self> {
        let bus = MessageBus::new();
        let registry = StateRegistry::in_memory().await?;
        let modules = ModuleManager::new(bus.clone());
        Ok(Self {
            bus,
            modules,
            registry,
        })
    }

    /// Build a kernel backed by an on-disk state registry at `path`.
    pub async fn with_registry(path: &str) -> Result<Self> {
        let bus = MessageBus::new();
        let registry = StateRegistry::connect(path).await?;
        let modules = ModuleManager::new(bus.clone());
        Ok(Self {
            bus,
            modules,
            registry,
        })
    }

    /// Handle to the message bus.
    pub fn bus(&self) -> &MessageBus {
        &self.bus
    }

    /// Handle to the module manager.
    pub fn modules(&self) -> &ModuleManager {
        &self.modules
    }

    /// Handle to the global state registry.
    pub fn registry(&self) -> &StateRegistry {
        &self.registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kernel_wires_subsystems() {
        let kernel = Kernel::in_memory().await.unwrap();
        // Every subsystem is reachable and the registry has been migrated.
        assert_eq!(kernel.bus().subscriber_count().await, 0);
        assert!(kernel.registry().list_modules().await.unwrap().is_empty());
        assert!(kernel.modules().list().await.is_empty());
    }
}
