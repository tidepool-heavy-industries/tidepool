//! Ordinary compiler-output refusal must precede candidate publication.

use super::*;

fn candidate_records(cache: &Path) -> usize {
    let root = cache.join(RECORD_DIR);
    let mut pending = match fs::symlink_metadata(&root) {
        Ok(_) => vec![root],
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("candidate namespace cannot be observed: {error}"),
    };
    let mut records = 0;
    let mut visited = 0;
    while let Some(path) = pending.pop() {
        visited += 1;
        assert!(visited <= 4096, "bounded fixture candidate census");
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(!metadata.is_symlink());
        if metadata.is_dir() {
            pending.extend(
                fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        } else {
            assert!(metadata.is_file());
            if metadata.len() >= RECORD_MAGIC.len() as u64 {
                let mut prefix = [0; 8];
                fs::File::open(path)
                    .unwrap()
                    .read_exact(&mut prefix)
                    .unwrap();
                records += usize::from(&prefix == RECORD_MAGIC);
            }
        }
    }
    records
}

#[test]
fn refused_completed_output_keeps_candidate_store_empty_before_valid_followup() {
    struct RestoreCache(Option<std::ffi::OsString>);
    impl Drop for RestoreCache {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", value),
                None => std::env::remove_var("TIDEPOOL_COMPILE_CACHE_DIR"),
            }
        }
    }
    let directory = crate::artifacts::test_support::compiler_input_directory().unwrap();
    let cache = directory.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let _cache = RestoreCache(std::env::var_os("TIDEPOOL_COMPILE_CACHE_DIR"));
    std::env::set_var("TIDEPOOL_COMPILE_CACHE_DIR", &cache);
    let dependency = directory.path().join("ResponseDependency.hs");
    let source = include_str!("../../tests/fixtures/completed-response/ResponseDependency.hs");
    let consumer = include_str!("../../tests/fixtures/completed-response/ResponseConsumer.hs");
    fs::write(&dependency, source).unwrap();
    let include = [directory.path().to_owned()];
    let invocation = crate::artifacts::CompileInvocation {
        source: consumer,
        targets: &["result"],
        include: &include,
        fallback_module_name: "ResponseConsumer",
    };
    assert_eq!(candidate_records(&cache), 0);
    let mut completed_requests = 0;
    let mut output_read = false;
    let refused = crate::artifacts::test_support::compile_invocation(&invocation, |stage, _, _| {
        if stage == crate::timing::STAGE_EXTRACT_SPAWN {
            completed_requests += 1;
        }
        if stage == crate::timing::STAGE_CBOR_READ && !output_read {
            output_read = true;
            // The actual worker completed. Its consumed source can no longer
            // authenticate the output that would publish ordinary suggestions.
            fs::write(&dependency, source.replace("41", "42")).unwrap();
        }
    });
    assert!(output_read);
    assert_eq!(
        completed_requests, 1,
        "output refusal cannot replay the request"
    );
    assert!(matches!(refused,
        Err(crate::CompileError::CompilerEvidence(error))
            if matches!(*error, crate::certified_products::CertificationError::CompletedSourceEvidence { .. })));
    assert_eq!(
        candidate_records(&cache),
        0,
        "refused completed output must not publish candidate records"
    );
    let valid = crate::artifacts::test_support::compile_invocation(&invocation, |_, _, _| {})
        .expect("a new actual request may compile the changed source");
    assert!(
        candidate_records(&cache) > 0,
        "a valid follow-up must publish actual ordinary support suggestions"
    );
    let selected = select_configured(
        valid.producer_identity.as_ref().unwrap(),
        &include,
        &directory.path().join("selected-followup"),
    )
    .unwrap()
    .unwrap();
    assert!(
        selected
            .by_owner
            .contains_key(&("main".into(), "ResponseDependency".into())),
        "the positive control acquires the actual published dependency candidate"
    );
}
