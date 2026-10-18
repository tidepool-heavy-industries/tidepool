use super::*;
use crate::ActorEffectKey as Key;
use exomonad_tool::ToolEffectKey;

#[test]
fn admitted_explicit_row_preserves_order_and_duplicates() {
    let requested = vec![Key::Reflect, Key::Commands, Key::Reflect];
    // Intrinsic and handler support can overlap and arrive in any order.
    let support = vec![
        ToolEffectKey::Actor(Key::Commands),
        ToolEffectKey::Actor(Key::Reflect),
        ToolEffectKey::Actor(Key::Commands),
        ToolEffectKey::ContextReadWrite,
    ];
    assert_eq!(
        admit_explicit_effects(requested.clone(), &support).unwrap(),
        requested
    );
}

#[test]
fn refused_explicit_row_names_every_unavailable_interpreter() {
    let requested = vec![
        Key::Reflect,
        Key::Jev,
        Key::ModelCallDeepSeek,
        Key::ModelCallArgon,
    ];
    for (supported, expected) in [
        (
            vec![],
            vec![
                Key::Reflect,
                Key::Jev,
                Key::ModelCallDeepSeek,
                Key::ModelCallArgon,
            ],
        ),
        (
            vec![Key::Reflect],
            vec![Key::Jev, Key::ModelCallDeepSeek, Key::ModelCallArgon],
        ),
        (
            vec![Key::Reflect, Key::Jev],
            vec![Key::ModelCallDeepSeek, Key::ModelCallArgon],
        ),
        (
            vec![Key::Reflect, Key::Jev, Key::ModelCallDeepSeek],
            vec![Key::ModelCallArgon],
        ),
    ] {
        let support = supported
            .iter()
            .copied()
            .map(ToolEffectKey::Actor)
            .collect::<Vec<_>>();
        let error = admit_explicit_effects(requested.clone(), &support).unwrap_err();
        let ResidentActorWorkbenchError::UnavailableInterpreterSupport { unavailable } = error
        else {
            panic!("unsupported interpreters must retain their typed effect keys")
        };
        assert_eq!(unavailable, expected);
    }
}
