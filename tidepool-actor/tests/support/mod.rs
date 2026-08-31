use tidepool_repr::SessionId;

/// Makes integration-test session namespaces distinct across test processes.
pub fn process_unique_session(discriminator: u32) -> SessionId {
    SessionId((u64::from(std::process::id()) << 32) | u64::from(discriminator))
}
