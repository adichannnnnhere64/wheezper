//! Meeting-aware assistant: bounded context retrieval plus a pluggable LLM
//! provider.

pub mod context;
pub mod provider;
pub mod repository;
pub mod service;

pub use service::AssistantService;
