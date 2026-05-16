//! Oxide Kernel demo binary.
//!
//! Boots the kernel, registers a tiny "echo" module, sends a few messages
//! through the bus, persists a configuration value to the registry, and prints
//! the resulting state. Useful as a smoke test and as living documentation
//! for the kernel API.

use async_trait::async_trait;
use oxide_k::bus::{Command, Event, Message};
use oxide_k::module::{Module, ModuleKind, ModuleMetadata};
use oxide_k::{Kernel, Result};
use tracing::{info, Level};

/// Trivial in-process module used by the demo. It does not actually respond to
/// commands itself — the demo task below watches the bus and answers on its
/// behalf — but it does demonstrate the lifecycle hooks.
struct EchoModule;

#[async_trait]
impl Module for EchoModule {
    fn metadata(&self) -> ModuleMetadata {
        ModuleMetadata {
            id: "echo".into(),
            name: "Echo".into(),
            version: "0.1.0".into(),
            kind: ModuleKind::Native,
            description: Some("Demo module that echoes Ping commands.".into()),
        }
    }

    async fn start(&mut self) -> Result<()> {
        info!(module = "echo", "starting");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!(module = "echo", "stopping");
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_target(false)
        .init();

    info!("Booting Oxide Kernel...");
    let kernel = Kernel::in_memory().await?;

    // Subscribe to the bus *before* starting modules so we don't miss events.
    let mut sub = kernel.bus().subscribe().await;

    // Register the echo module.
    kernel.modules().register_native(EchoModule).await?;
    kernel.modules().init("echo").await?;

    // Persist the module's metadata in the registry, then update its state as
    // it transitions through the lifecycle.
    let meta = ModuleMetadata {
        id: "echo".into(),
        name: "Echo".into(),
        version: "0.1.0".into(),
        kind: ModuleKind::Native,
        description: Some("Demo module that echoes Ping commands.".into()),
    };
    kernel
        .registry()
        .upsert_module(&meta, oxide_k::module::ModuleState::Loaded)
        .await?;

    // Stash a piece of configuration to show the registry round-trips JSON.
    kernel
        .registry()
        .set_config("kernel.greeting", &"Hello, agent.")
        .await?;
    let greeting: String = kernel.registry().get_config("kernel.greeting").await?;
    info!(greeting, "loaded greeting from registry");

    // Drive the echo module through start -> ping -> stop.
    kernel.modules().start("echo").await?;
    kernel
        .registry()
        .set_module_state("echo", oxide_k::module::ModuleState::Running)
        .await?;

    // Spawn a tiny task that "is" the echo module on the bus: it watches for
    // Ping commands and emits a Pong event. Real modules would do this inside
    // their own `start` hook.
    let bus_clone = kernel.bus().clone();
    let mut module_sub = bus_clone.subscribe().await;
    let echo_task = tokio::spawn(async move {
        while let Some(envelope) = module_sub.receiver.recv().await {
            if let Message::Command(Command::Ping) = envelope.message {
                let _ = bus_clone
                    .emit_event(
                        "echo",
                        Event::Pong {
                            from: "echo".into(),
                        },
                    )
                    .await;
                break;
            }
        }
    });

    // Send a ping from the kernel itself.
    kernel.bus().send_command("kernel", Command::Ping).await?;

    // Drain a few events from the subscriber to show messages flowing.
    for _ in 0..3 {
        if let Some(env) = sub.receiver.recv().await {
            info!(?env.source, ?env.message, "bus event");
            if matches!(env.message, Message::Event(Event::Pong { .. })) {
                break;
            }
        }
    }

    // Tear down.
    kernel.modules().stop("echo").await?;
    kernel
        .registry()
        .set_module_state("echo", oxide_k::module::ModuleState::Stopped)
        .await?;
    let _ = echo_task.await;

    info!(
        modules = ?kernel.registry().list_modules().await?,
        "final registry state"
    );

    info!("Kernel demo complete.");
    Ok(())
}
