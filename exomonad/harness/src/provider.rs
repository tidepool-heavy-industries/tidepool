//! Calling-model provider adapters. The neutral request, response, streaming,
//! error, and call traits live in `tidepool-model`; this module contains the
//! ChatGPT-subscription OAuth and API-key implementations plus their settings.
//! This is not the in-program `Llm` effect (`tidepool-handlers`): it has a
//! different consumer and budget contract.
//!
//! The API-key impl routes chat calls through `genai` (`http` submodule);
//! the OAuth impl hand-rolls its own Responses-API call instead (see
//! `oauth`'s module doc for why genai can't drive it), though its login
//! mechanics still ride the `openai-auth` crate rather than a hand-rolled
//! PKCE flow.

pub mod api_key;
pub(crate) mod http;
pub mod oauth;
pub(crate) mod paths;
pub mod settings;

pub use tidepool_model::{
    DynModelProvider, Message, ModelProvider, ProviderError, ReasoningItem, Role, StreamDelta,
    StreamSink, TurnRequest, TurnResponse, Usage,
};
