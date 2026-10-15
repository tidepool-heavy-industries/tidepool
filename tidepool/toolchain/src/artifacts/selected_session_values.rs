//! Complete ordered value-owner metadata for one physical compiler request.
use std::{collections::HashSet, ffi::OsStr, sync::Arc};
use tidepool_repr::{SessionModule, SessionModuleKind};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionValueSelectionError {
    #[error("selected session value {index} of {total} is not a UTF-8 nonzero Val owner")]
    InvalidOwner { index: usize, total: usize },
    #[error("selected session value {index} of {total} repeats owner {owner:?}")]
    DuplicateOwner {
        index: usize,
        total: usize,
        owner: SessionModule,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedSessionValueOwners(Arc<[SessionModule]>);

impl SelectedSessionValueOwners {
    pub(crate) fn from_command(
        command: &tidepool_extract_cmd::ExtractCmd,
    ) -> Result<Self, SessionValueSelectionError> {
        Self::capture(command.selected_session_values())
    }

    pub(crate) fn capture<'a>(
        names: impl IntoIterator<Item = &'a OsStr>,
    ) -> Result<Self, SessionValueSelectionError> {
        let names = names.into_iter().collect::<Vec<_>>();
        let total = names.len();
        let mut selected = Vec::with_capacity(total);
        let mut seen = HashSet::with_capacity(total);
        for (index, name) in names.into_iter().enumerate() {
            let owner = name
                .to_str()
                .and_then(SessionModule::from_module_name)
                .filter(|owner| owner.kind == SessionModuleKind::Val && owner.gen.0 != 0)
                .ok_or(SessionValueSelectionError::InvalidOwner { index, total })?;
            if !seen.insert(owner) {
                return Err(SessionValueSelectionError::DuplicateOwner {
                    index,
                    total,
                    owner,
                });
            }
            selected.push(owner);
        }
        Ok(Self(selected.into()))
    }

    pub(crate) fn as_slice(&self) -> &[SessionModule] {
        &self.0
    }

    pub(crate) fn contains(&self, owner: &SessionModule) -> bool {
        self.0.contains(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tidepool_extract_cmd::{ExtractCmd, ExtractRequest, ResolvedExtractBin};

    fn command() -> ExtractCmd {
        ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("unused-worker"))
    }

    #[test]
    fn complete_selection_preserves_every_owner_across_old_count_boundary() {
        for count in [0, 1, 127, 128, 129, 257] {
            let mut request = command();
            let expected = (1..=count)
                .rev()
                .map(|generation| SessionModule::val(tidepool_repr::Generation(generation)))
                .collect::<Vec<_>>();
            request.inject_vals(expected.iter().map(SessionModule::module_name));
            let selected = SelectedSessionValueOwners::from_command(&request).unwrap();
            assert_eq!(selected.as_slice(), expected);
            let transported = ExtractRequest::decode(&request.request_bytes()).unwrap();
            assert_eq!(transported.encode(), request.request_bytes());
            let retained = selected.clone();
            request.inject_val("Tidepool.Session.Val.G1001");
            assert_eq!(retained.as_slice(), expected);
            assert!(Arc::ptr_eq(&selected.0, &retained.0));
        }
    }

    #[test]
    fn complete_selection_refuses_invalid_and_semantically_duplicate_owners() {
        use std::os::unix::ffi::OsStrExt;
        for invalid in [
            OsStr::new("Tidepool.Session.Val.G0"),
            OsStr::new("Tidepool.Session.Lib.G1"),
            OsStr::new("Other"),
            OsStr::from_bytes(b"Tidepool.Session.Val.G\xff"),
        ] {
            assert_eq!(
                SelectedSessionValueOwners::capture([
                    OsStr::new("Tidepool.Session.Val.G1"),
                    invalid
                ]),
                Err(SessionValueSelectionError::InvalidOwner { index: 1, total: 2 }),
            );
        }
        assert!(matches!(
            SelectedSessionValueOwners::capture([
                OsStr::new("Tidepool.Session.Val.G1"),
                OsStr::new("Tidepool.Session.Val.G01"),
            ]),
            Err(SessionValueSelectionError::DuplicateOwner {
                index: 1,
                total: 2,
                ..
            }),
        ));
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(
                "proptest-regressions/selected_session_values.txt",
            ))),
            ..ProptestConfig::default()
        })]
        #[test]
        fn selection_histories_preserve_complete_snapshots_and_refusals(
            initial in 120usize..140,
            edits in proptest::collection::vec((any::<bool>(), 0u16..300), 1..32),
        ) {
            let mut request = command();
            let mut names = (1..=initial as u64).collect::<Vec<_>>();
            request.inject_vals(names.iter().map(|generation| format!("Tidepool.Session.Val.G{generation}")));
            let frozen = SelectedSessionValueOwners::from_command(&request).unwrap();
            for (fresh, value) in edits {
                let owner = if fresh { names.iter().copied().max().unwrap() + 1 } else { u64::from(value) };
                names.push(owner);
                request.inject_val(format!("Tidepool.Session.Val.G{owner}"));
                let result = SelectedSessionValueOwners::from_command(&request);
                // The oracle uses logical generations and the original ordered list,
                // independently of module parsing and the production hash set.
                let first_bad = names.iter().enumerate().find_map(|(index, owner)| {
                    if *owner == 0 { Some((index, false)) }
                    else if names[..index].contains(owner) { Some((index, true)) }
                    else { None }
                });
                match first_bad {
                    Some((index, false)) => prop_assert_eq!(result, Err(SessionValueSelectionError::InvalidOwner { index, total: names.len() })),
                    Some((index, true)) => prop_assert_eq!(result, Err(SessionValueSelectionError::DuplicateOwner {
                        index, total: names.len(), owner: SessionModule::val(tidepool_repr::Generation(names[index])),
                    })),
                    None => prop_assert_eq!(result.unwrap().as_slice().iter().map(|owner| owner.gen.0).collect::<Vec<_>>(), names.clone()),
                }
                prop_assert_eq!(frozen.as_slice().iter().map(|owner| owner.gen.0).collect::<Vec<_>>(), (1..=initial as u64).collect::<Vec<_>>());
            }
        }
    }
}
