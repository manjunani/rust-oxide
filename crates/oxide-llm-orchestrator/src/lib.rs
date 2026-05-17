//! # `oxide-llm-orchestrator` — LLM Front-Door for Rust Oxide
//!
//! Provider-agnostic client for OpenAI-compatible chat-completion endpoints
//! (OpenAI, Anthropic via proxy, OpenRouter, Ollama, vLLM, …) plus the
//! prompt-engineering and feedback-loop logic that ties LLMs into the rest of
//! the Rust Oxide stack.
//!
//! Layout:
//!
//! * [`client`] — [`LlmClient`](client::LlmClient) async trait,
//!   [`OpenAiClient`](client::OpenAiClient) HTTP implementation, and
//!   [`MockLlmClient`](client::MockLlmClient) for offline tests.
//! * [`prompts`] — reusable system / user prompt templates for the recurring
//!   tasks: healing browser failures, analysing errors, summarising long
//!   contexts.
//! * [`healing`] — [`LlmHealing`](healing::LlmHealing) implements
//!   [`oxide_browser_sh::HealingStrategy`] and routes the failure context
//!   produced by `oxide-browser-sh` through an [`LlmClient`].
//! * [`summarize`] — convenience wrapper around the summarisation prompt.
//! * [`kernel`] — [`LlmModule`](kernel::LlmModule) exposes the orchestrator
//!   on the kernel message bus.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod client;
pub mod error;
pub mod healing;
pub mod kernel;
pub mod prompts;
pub mod summarize;

pub use client::{
    ChatMessage, ChatRequest, ChatResponse, ChatRole, LlmClient, MockLlmClient, OpenAiClient,
    ResponseFormat,
};
pub use error::{LlmError, Result};
pub use healing::LlmHealing;
pub use kernel::LlmModule;
pub use prompts::{HealingPromptInput, PromptTemplate, ErrorAnalysisInput, SummarizeInput};
pub use summarize::Summarizer;
