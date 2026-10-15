use super::*;

#[tokio::test]
async fn native_archive_text_retains_exact_observations_and_replays_without_effects() {
    let fixture = Fixture::new(756, false).await;
    fixture
        .execute(&tidepool_testing::fixture_source(
            "exomonad/actor/src/resident_actor/jev_recording_semantics.hs",
        ))
        .await;
    assert!(fixture.jev.requests.lock().is_empty());
    assert!(fixture.host.0.lock().views.is_empty());
    fixture.finish().await;
}
