# tidepool-model-output — model text protocols

This crate owns provider-neutral parsing of model-authored response text. It
does not call providers, execute Haskell, own conversations, or assign runtime
meaning beyond identifying explicitly marked output regions.

Keep parsers deterministic, allocation-conscious, and covered by behavioral
edge-case tests. Provider transport belongs in provider/backend crates;
execution policy belongs in the consuming runtime.
