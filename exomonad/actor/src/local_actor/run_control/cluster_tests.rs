use super::*;

#[tokio::test]
async fn reservation_issued_before_seal_cannot_authorize_later_reservation() {
    let (control, _task) = RunControlClient::start().await.unwrap();
    let reservation = control.reserve(crate::Incarnation(1)).await.unwrap();
    let issued = reservation.identity();
    let membership = control.seal().await.unwrap();
    assert!(membership.actors.is_empty());
    assert!(matches!(control.reserve(crate::Incarnation(1)).await, Err(DirectoryError::Closed)));
    assert!(control.resolve(issued).await.unwrap().is_none());
    drop(reservation);
    assert!(control.seal().await.unwrap().actors.is_empty());
    control.owner.stop(None);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn reservation_and_seal_histories_preserve_issued_identity_and_cutoff(
        operations in proptest::collection::vec((0u8..3, 0usize..3), 1..24)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let (control, _task) = RunControlClient::start().await.unwrap();
            let mut running = true;
            let mut issued = std::collections::HashSet::new();
            let mut held: [Option<ActorReservation>; 3] = std::array::from_fn(|_| None);
            for (operation, slot) in operations {
                match operation {
                    0 => {
                        let result = control.reserve(crate::Incarnation(1)).await;
                        assert_eq!(result.is_ok(), running);
                        if let Ok(reservation) = result {
                            assert!(issued.insert(reservation.identity()), "identity issued twice");
                            held[slot] = Some(reservation);
                        } else {
                            assert!(matches!(result, Err(DirectoryError::Closed)));
                        }
                    }
                    1 => { held[slot].take(); }
                    _ => {
                        assert!(control.seal().await.unwrap().actors.is_empty());
                        running = false;
                    }
                }
            }
            drop(held);
            control.seal().await.unwrap();
            control.owner.stop(None);
        });
    }
}
