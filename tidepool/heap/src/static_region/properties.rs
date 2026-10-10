use super::*;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, FileFailurePersistence, TestCaseError};
use std::{cell::RefCell, sync::Arc};
use tidepool_repr::execution_schema::{Architecture, Endianness, StorageLayout, TargetDescriptor};

const REGIONS: usize = 8;
const MAX_OBJECTS: usize = 4;
const OBJECT_BYTES: usize = 16;

struct PoolRegion {
    region: Arc<StaticRegion>,
    start: usize,
    bytes: usize,
    objects: usize,
    tag: usize,
    constructor: usize,
}

fn pool(counts: &[u8; REGIONS]) -> Vec<PoolRegion> {
    let target = TargetDescriptor {
        architecture: Architecture::X86_64,
        endianness: Endianness::Little,
        pointer_width: 64,
        word_width: 64,
        abi: "system-v".into(),
        features: Vec::new(),
    };
    counts
        .iter()
        .enumerate()
        .map(|(index, &count)| {
            let descriptor = Arc::new(
                ObjectDescriptor::constructor(
                    index as u32 + 1,
                    StorageLayout::for_reps(&target, &[]).unwrap(),
                    None,
                )
                .unwrap(),
            );
            let words = (count as usize) * (OBJECT_BYTES / 8);
            let mut image_words = vec![0; words];
            for object in 0..count as usize {
                image_words[object * (OBJECT_BYTES / 8)] = descriptor.initial_header_word() as u64;
            }
            let image = StaticImage::new(
                image_words,
                vec![],
                BTreeMap::new(),
                [Arc::clone(&descriptor)],
            )
            .unwrap();
            let region = Arc::new(image.instantiate().unwrap());
            let start = region.words.as_ptr() as usize;
            PoolRegion {
                region,
                start,
                bytes: count as usize * OBJECT_BYTES,
                objects: count as usize,
                tag: descriptor.tag() as usize,
                constructor: index + 1,
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Insert(usize),
    Remove(usize),
    RemoveStart(usize),
    Admit { region: usize, point: u8 },
    AdmitTagged { region: usize, point: u8, tag: u8 },
    AdmitInterior { region: usize, object: usize },
    Overlaps { region: usize, point: u8 },
}

fn operations() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            (0..REGIONS).prop_map(Op::Insert),
            (0..REGIONS).prop_map(Op::Remove),
            (0..REGIONS).prop_map(Op::RemoveStart),
            (0..REGIONS, 0u8..5).prop_map(|(region, point)| Op::Admit { region, point }),
            (0..REGIONS, 0u8..5, 0u8..8).prop_map(|(region, point, tag)| Op::AdmitTagged {
                region,
                point,
                tag
            }),
            (0..REGIONS, 0..MAX_OBJECTS)
                .prop_map(|(region, object)| Op::AdmitInterior { region, object }),
            (0..REGIONS, 0u8..7).prop_map(|(region, point)| Op::Overlaps { region, point }),
        ],
        0..100,
    )
}

fn property_config() -> Config {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 128;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

#[derive(Debug, Default)]
struct Coverage {
    tag_refused: usize,
    untagged_admitted: usize,
    descriptor_tag_admitted: usize,
    tagged_outside: usize,
    insert_new: usize,
    insert_duplicate: usize,
    insert_empty: usize,
    remove_hit: usize,
    remove_miss: usize,
    remove_start_hit: usize,
    remove_start_miss: usize,
    admit_hit: usize,
    admit_miss: usize,
    admit_interior: usize,
    overlap_true: usize,
    overlap_false: usize,
    empty_states: usize,
    single_region_states: usize,
    multiple_region_states: usize,
    max_regions: usize,
    insert_before_existing: usize,
}

impl Coverage {
    fn add(&mut self, other: Self) {
        self.tag_refused += other.tag_refused;
        self.untagged_admitted += other.untagged_admitted;
        self.descriptor_tag_admitted += other.descriptor_tag_admitted;
        self.tagged_outside += other.tagged_outside;
        self.insert_new += other.insert_new;
        self.insert_duplicate += other.insert_duplicate;
        self.insert_empty += other.insert_empty;
        self.remove_hit += other.remove_hit;
        self.remove_miss += other.remove_miss;
        self.remove_start_hit += other.remove_start_hit;
        self.remove_start_miss += other.remove_start_miss;
        self.admit_hit += other.admit_hit;
        self.admit_miss += other.admit_miss;
        self.admit_interior += other.admit_interior;
        self.overlap_true += other.overlap_true;
        self.overlap_false += other.overlap_false;
        self.empty_states += other.empty_states;
        self.single_region_states += other.single_region_states;
        self.multiple_region_states += other.multiple_region_states;
        self.max_regions = self.max_regions.max(other.max_regions);
        self.insert_before_existing += other.insert_before_existing;
    }

    fn observe_state(&mut self, regions: usize) {
        self.max_regions = self.max_regions.max(regions);
        match regions {
            0 => self.empty_states += 1,
            1 => self.single_region_states += 1,
            _ => self.multiple_region_states += 1,
        }
    }
}

fn probe_address(region: &PoolRegion, point: u8) -> (usize, usize) {
    if region.objects == 0 {
        return (0, 0);
    }
    match point {
        0 => (region.start, region.tag),
        1 => (
            region.start + (region.objects - 1) * OBJECT_BYTES,
            region.tag,
        ),
        2 => (region.start + region.bytes, 0),
        3 => (0, 0),
        _ => (usize::MAX - 7, 0),
    }
}

fn slot_address(region: &PoolRegion, point: u8) -> usize {
    if region.objects == 0 {
        return usize::MAX - 7;
    }
    match point {
        0 => region.start.saturating_sub(8),
        1 => region.start.saturating_sub(7),
        2 => region.start,
        3 => region.start + region.bytes - 8,
        4 => region.start + region.bytes - 7,
        5 => region.start + region.bytes,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    Outside,
    Interior(usize),
    WrongTag(usize, usize),
    Admitted(usize),
}

fn model_admit(pool: &[PoolRegion], active: &[usize], encoded: usize) -> Admission {
    let address = encoded & !7;
    let tag = encoded & 7;
    for &id in active {
        let region = &pool[id];
        if region.objects == 0 || address < region.start || address >= region.start + region.bytes {
            continue;
        }
        if (address - region.start) % OBJECT_BYTES != 0 {
            return Admission::Interior(address);
        }
        // These nullary constructor facts come from the fixture operand.
        // The ABI accepts zero as inconclusive and seven as a live descriptor tag.
        if tag != 0 && tag != 7 && tag != region.constructor {
            return Admission::WrongTag(address, tag);
        }
        return Admission::Admitted(id);
    }
    Admission::Outside
}

fn check_admission(
    catalog: &StaticRegionCatalog,
    metrics: &StaticLookupMetrics,
    pool: &[PoolRegion],
    active: &[usize],
    encoded: usize,
) -> Result<Admission, TestCaseError> {
    let expected = model_admit(pool, active, encoded);
    let actual = catalog.admit(encoded, metrics);
    match (expected, actual) {
        (Admission::Outside, Ok(None)) => {}
        (Admission::Admitted(id), Ok(Some(region))) => {
            prop_assert!(std::ptr::eq(region, pool[id].region.as_ref()));
        }
        (
            Admission::Interior(want),
            Err(DescriptorTraceError::InvalidManagedPointer { address }),
        ) => {
            prop_assert_eq!(address, want);
        }
        (
            Admission::WrongTag(want_address, want_tag),
            Err(DescriptorTraceError::InvalidManagedTag { address, tag }),
        ) => {
            prop_assert_eq!((address, tag as usize), (want_address, want_tag));
        }
        (expected, actual) => {
            return Err(TestCaseError::fail(format!(
                "encoded={encoded:#x} expected={expected:?} actual={:?}",
                actual.map(|region| region.is_some())
            )))
        }
    }
    Ok(expected)
}

fn model_overlap(pool: &[PoolRegion], active: &[usize], address: usize) -> bool {
    let Some(end) = address.checked_add(std::mem::size_of::<*mut u8>()) else {
        return true;
    };
    active.iter().copied().any(|id| {
        let region = &pool[id];
        region.objects != 0 && address < region.start + region.bytes && region.start < end
    })
}

fn run_history(counts: [u8; REGIONS], history: &[Op]) -> Result<Coverage, TestCaseError> {
    let pool = pool(&counts);
    let mut catalog = StaticRegionCatalog::new();
    let mut active = Vec::<usize>::new();
    let metrics = StaticLookupMetrics::new("static-region-property");
    let mut coverage = Coverage::default();
    coverage.observe_state(active.len());

    for op in history {
        match *op {
            Op::Insert(id) => {
                let region = &pool[id];
                let was_active = active.contains(&id);
                let result = catalog.insert(Arc::clone(&region.region));
                if region.objects == 0 {
                    prop_assert_eq!(result.unwrap(), false);
                    coverage.insert_empty += 1;
                } else if was_active {
                    prop_assert_eq!(result.unwrap(), false);
                    coverage.insert_duplicate += 1;
                } else {
                    if active
                        .iter()
                        .any(|&active_id| pool[active_id].start > region.start)
                    {
                        coverage.insert_before_existing += 1;
                    }
                    prop_assert_eq!(result.unwrap(), true);
                    active.push(id);
                    coverage.insert_new += 1;
                }
            }
            Op::Remove(id) => {
                let position = active.iter().position(|&active_id| active_id == id);
                let removed = catalog.remove(&pool[id].region);
                prop_assert_eq!(removed, position.is_some());
                if let Some(position) = position {
                    active.remove(position);
                    coverage.remove_hit += 1;
                } else {
                    coverage.remove_miss += 1;
                }
            }
            Op::RemoveStart(id) => {
                let region = &pool[id];
                let start = if region.objects == 0 { 0 } else { region.start };
                let position = active.iter().position(|&active_id| {
                    let candidate = &pool[active_id];
                    candidate.objects != 0 && candidate.start == start
                });
                let removed = catalog.remove_start(start);
                prop_assert_eq!(removed, position.is_some());
                if let Some(position) = position {
                    active.remove(position);
                    coverage.remove_start_hit += 1;
                } else {
                    coverage.remove_start_miss += 1;
                }
            }
            Op::Admit { region, point } => {
                let fact = &pool[region];
                let (address, tag) = probe_address(fact, point);
                let encoded = address | tag;
                let expected = check_admission(&catalog, &metrics, &pool, &active, encoded)?;
                if matches!(expected, Admission::Admitted(_)) {
                    coverage.admit_hit += 1;
                } else {
                    coverage.admit_miss += 1;
                }
            }
            Op::AdmitTagged { region, point, tag } => {
                let (address, _) = probe_address(&pool[region], point);
                let expected =
                    check_admission(&catalog, &metrics, &pool, &active, address | tag as usize)?;
                match expected {
                    Admission::WrongTag(_, _) => coverage.tag_refused += 1,
                    Admission::Admitted(_) if tag == 0 => coverage.untagged_admitted += 1,
                    Admission::Admitted(_) if tag == 7 => coverage.descriptor_tag_admitted += 1,
                    Admission::Outside => coverage.tagged_outside += 1,
                    _ => {}
                }
            }
            Op::AdmitInterior { region, object } => {
                let fact = &pool[region];
                if fact.objects != 0 {
                    let object = object % fact.objects;
                    let address = fact.start + object * OBJECT_BYTES + 8;
                    prop_assert!(address >= fact.start && address < fact.start + fact.bytes);
                    prop_assert!((address - fact.start) % OBJECT_BYTES != 0);
                    check_admission(&catalog, &metrics, &pool, &active, address)?;
                    if !active.contains(&region) {
                        prop_assert!(catalog.admit(address, &metrics).unwrap().is_none());
                    } else {
                        let actual = catalog.admit(address, &metrics);
                        prop_assert!(
                            matches!(
                                actual,
                                Err(DescriptorTraceError::InvalidManagedPointer { address: bad })
                                    if bad == address
                            ),
                            "active interior address must report its exact invalid managed pointer"
                        );
                        coverage.admit_interior += 1;
                    }
                }
            }
            Op::Overlaps { region, point } => {
                let address = slot_address(&pool[region], point);
                let expected = model_overlap(&pool, &active, address);
                let actual = catalog.overlaps_slot(address);
                prop_assert_eq!(actual, expected);
                if actual {
                    coverage.overlap_true += 1;
                } else {
                    coverage.overlap_false += 1;
                }
            }
        }
        prop_assert_eq!(catalog.len(), active.len());
        prop_assert_eq!(catalog.is_empty(), active.is_empty());
        coverage.observe_state(active.len());
        for fact in &pool {
            for object in 0..fact.objects {
                let encoded = (fact.start + object * OBJECT_BYTES) | fact.tag;
                check_admission(&catalog, &metrics, &pool, &active, encoded)?;
            }
        }
    }
    Ok(coverage)
}

#[test]
fn catalog_histories_match_linear_region_facts() {
    let mut config = property_config();
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::catalog_histories_match_linear_region_facts"
    ));
    let configured_cases = config.cases;
    let callbacks = RefCell::new(0usize);
    let completed = RefCell::new(0usize);
    let coverage = RefCell::new(Coverage::default());
    let initial_failure = RefCell::new(None);
    let strategy = (
        prop::array::uniform::<_, REGIONS>(0u8..=MAX_OBJECTS as u8),
        operations(),
    );
    let result =
        proptest::test_runner::TestRunner::new(config).run(&strategy, |(mut counts, tail)| {
            *callbacks.borrow_mut() += 1;
            let input = (counts, tail.clone());
            counts[0] = counts[0].max(1);
            let mut history = vec![
                Op::Insert(0),
                Op::AdmitTagged {
                    region: 0,
                    point: 0,
                    tag: 2,
                },
                Op::AdmitTagged {
                    region: 0,
                    point: 0,
                    tag: 0,
                },
                Op::AdmitTagged {
                    region: 0,
                    point: 0,
                    tag: 7,
                },
                Op::Remove(0),
                Op::AdmitTagged {
                    region: 0,
                    point: 0,
                    tag: 2,
                },
                Op::Insert(0),
                Op::AdmitTagged {
                    region: 0,
                    point: 0,
                    tag: 2,
                },
            ];
            history.extend(tail);
            match run_history(counts, &history) {
                Ok(observed) => {
                    coverage.borrow_mut().add(observed);
                    *completed.borrow_mut() += 1;
                    Ok(())
                }
                Err(error) => {
                    if initial_failure.borrow().is_none() {
                        *initial_failure.borrow_mut() = Some(input);
                    }
                    Err(error)
                }
            }
        });
    eprintln!("static_region_campaign configured_cases={configured_cases} callbacks={} completed={} coverage={:?} initial_failure={:?} minimized={:?}", callbacks.borrow(), completed.borrow(), coverage.borrow(), initial_failure.borrow(), result);
    result.unwrap();
    let observed = coverage.into_inner();
    assert!(observed.tag_refused >= *completed.borrow() * 2);
    assert!(observed.untagged_admitted >= *completed.borrow());
    assert!(observed.descriptor_tag_admitted >= *completed.borrow());
    assert!(observed.tagged_outside >= *completed.borrow());
}

#[test]
fn targeted_history_reaches_catalog_transitions_and_boundaries() {
    let counts = [1, 2, 0, 1, 3, 0, 2, 1];
    let history = [
        Op::Insert(1),
        Op::Insert(1),
        Op::Insert(2),
        Op::Insert(0),
        Op::Insert(3),
        Op::Admit {
            region: 1,
            point: 0,
        },
        Op::Admit {
            region: 1,
            point: 1,
        },
        Op::Admit {
            region: 1,
            point: 2,
        },
        Op::Admit {
            region: 1,
            point: 3,
        },
        Op::AdmitInterior {
            region: 1,
            object: 0,
        },
        Op::Admit {
            region: 2,
            point: 0,
        },
        Op::Overlaps {
            region: 1,
            point: 0,
        },
        Op::Overlaps {
            region: 1,
            point: 1,
        },
        Op::Overlaps {
            region: 1,
            point: 3,
        },
        Op::Overlaps {
            region: 1,
            point: 4,
        },
        Op::Overlaps {
            region: 1,
            point: 6,
        },
        Op::RemoveStart(1),
        Op::RemoveStart(1),
        Op::Insert(1),
        Op::Remove(0),
        Op::Remove(3),
        Op::Remove(0),
    ];
    let coverage = run_history(counts, &history).unwrap();
    assert!(coverage.insert_new >= 3);
    assert!(coverage.insert_duplicate >= 1);
    assert!(coverage.insert_empty >= 1);
    assert!(coverage.remove_hit >= 2);
    assert!(coverage.remove_miss >= 1);
    assert!(coverage.remove_start_hit >= 1);
    assert!(coverage.remove_start_miss >= 1);
    assert!(coverage.admit_hit >= 2);
    assert!(coverage.admit_miss >= 2);
    assert!(coverage.admit_interior >= 1);
    assert!(coverage.overlap_true >= 1);
    assert!(coverage.overlap_false >= 1);
    assert!(coverage.empty_states > 0);
    assert!(coverage.single_region_states > 0);
    assert!(coverage.multiple_region_states > 0);
    assert!(coverage.max_regions >= 2);
}

#[test]
fn deterministic_generated_histories_reach_catalog_outcomes() {
    let counts_strategy = prop::array::uniform::<_, REGIONS>(0u8..=MAX_OBJECTS as u8);
    let history_strategy = operations();
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let mut coverage = Coverage::default();

    for _ in 0..64 {
        let counts = counts_strategy.new_tree(&mut runner).unwrap().current();
        let history = history_strategy.new_tree(&mut runner).unwrap().current();
        coverage.add(run_history(counts, &history).unwrap());
    }

    println!("static-region generated support: {coverage:?}");
    assert!(coverage.insert_new > 0);
    assert!(coverage.insert_duplicate > 0);
    assert!(coverage.insert_empty > 0);
    assert!(coverage.remove_hit > 0);
    assert!(coverage.remove_miss > 0);
    assert!(coverage.remove_start_hit > 0);
    assert!(coverage.remove_start_miss > 0);
    assert!(coverage.admit_hit > 0);
    assert!(coverage.admit_miss > 0);
    assert!(coverage.admit_interior > 0);
    assert!(coverage.overlap_true > 0);
    assert!(coverage.overlap_false > 0);
    assert!(coverage.empty_states > 0);
    assert!(coverage.single_region_states > 0);
    assert!(coverage.multiple_region_states > 0);
}
