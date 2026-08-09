//! Shared `genai`-client plumbing for both provider impls. 60-auth's crate
//! survey ruled out hand-rolling chat request/response JSON: `genai` is
//! already a workspace dependency (it drives `LlmHandler` for the
//! in-program `Llm` effect — a DIFFERENT consumer of the same library, per
//! the SPEC's anti-pattern about not conflating the two). Both providers
//! resolve a bearer token themselves (env/file for API-key, load-refresh
//! for OAuth) and hand it here as a plain string — this module turns that
//! into a `genai::Client` and maps its errors/responses to our types.

use genai::chat::{ChatMessage, ChatOptions, ChatRequest, ChatResponse};
use genai::resolver::{AuthData, AuthResolver, Endpoint, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget};

use crate::provider::{Message, ProviderError, Role, TurnRequest, TurnResponse, Usage};

/// Build a client that authenticates every call with the given (already
/// resolved) bearer token. `base_url`, when set, overrides ONLY the
/// endpoint — model→adapter routing still flows through genai's normal
/// resolution. Tests set `base_url` to a local fixture server; production
/// callers pass `None` and get genai's real endpoint resolution for the
/// model name, rather than a guessed URL baked in here.
pub(crate) fn build_client(base_url: Option<String>, token: String) -> Client {
    let auth_resolver = AuthResolver::from_resolver_fn(
        move |_model_iden: ModelIden| -> genai::resolver::Result<Option<AuthData>> {
            Ok(Some(AuthData::from_single(token.clone())))
        },
    );
    let mut builder = Client::builder().with_auth_resolver(auth_resolver);
    if let Some(url) = base_url {
        let target_resolver = ServiceTargetResolver::from_resolver_fn(
            move |mut target: ServiceTarget| -> genai::resolver::Result<ServiceTarget> {
                target.endpoint = Endpoint::from_owned(url.clone());
                Ok(target)
            },
        );
        builder = builder.with_service_target_resolver(target_resolver);
    }
    builder.build()
}

pub(crate) fn to_chat_request(req: &TurnRequest) -> ChatRequest {
    ChatRequest::from_messages(
        req.messages
            .iter()
            .map(|m: &Message| match m.role {
                Role::System => ChatMessage::system(m.content.clone()),
                Role::User => ChatMessage::user(m.content.clone()),
                Role::Assistant => ChatMessage::assistant(m.content.clone()),
            })
            .collect(),
    )
}

pub(crate) fn chat_options(req: &TurnRequest) -> Option<ChatOptions> {
    req.max_tokens
        .map(|n| ChatOptions::default().with_max_tokens(n))
}

pub(crate) fn to_turn_response(resp: ChatResponse) -> Result<TurnResponse, ProviderError> {
    let text = resp
        .first_text()
        .ok_or_else(|| ProviderError::Api("provider returned no text content".to_string()))?
        .to_string();
    let usage = Usage {
        input_tokens: resp.usage.prompt_tokens.unwrap_or(0).max(0) as u64,
        output_tokens: resp.usage.completion_tokens.unwrap_or(0).max(0) as u64,
    };
    // genai's chat/completions path carries no reasoning-summary stream.
    Ok(TurnResponse {
        text,
        usage,
        reasoning: None,
        reasoning_items: Vec::new(),
    })
}

/// A 401/403 from the provider — regardless of which adapter surfaced it —
/// is always an auth failure (both auth modes bill the same upstream, per
/// the SPEC); genai's own missing-credentials errors (no resolver ran, or
/// it returned nothing) are auth failures too. Anything else is a generic
/// API error.
pub(crate) fn map_genai_err(e: genai::Error) -> ProviderError {
    use genai::Error as E;
    let is_auth = match &e {
        E::RequiresApiKey { .. } | E::NoAuthResolver { .. } | E::NoAuthData { .. } => true,
        E::WebModelCall { webc_error, .. } | E::WebAdapterCall { webc_error, .. } => matches!(
            webc_error,
            genai::webc::Error::ResponseFailedStatus { status, .. }
                if status.as_u16() == 401 || status.as_u16() == 403
        ),
        _ => false,
    };
    if is_auth {
        ProviderError::Auth(e.to_string())
    } else {
        ProviderError::Api(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_chat_request_maps_roles() {
        let req = TurnRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "be terse".into(),
                    reasoning_items: Vec::new(),
                },
                Message {
                    role: Role::User,
                    content: "hi".into(),
                    reasoning_items: Vec::new(),
                },
            ],
            max_tokens: None,
        };
        let chat_req = to_chat_request(&req);
        assert_eq!(chat_req.join_systems(), Some("be terse".to_string()));
    }

    #[test]
    fn chat_options_none_without_max_tokens() {
        let req = TurnRequest {
            messages: vec![],
            max_tokens: None,
        };
        assert!(chat_options(&req).is_none());
    }

    #[test]
    fn chat_options_sets_max_tokens() {
        let req = TurnRequest {
            messages: vec![],
            max_tokens: Some(64),
        };
        assert_eq!(chat_options(&req).unwrap().max_tokens, Some(64));
    }
}
