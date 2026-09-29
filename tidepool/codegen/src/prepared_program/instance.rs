//! Identity and immutable descriptor/static ownership of one code installation.

use super::{plan::HeapTopSpec, CompiledProgram, DescriptorMetadata, ExecutionError};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use tidepool_heap::execution_descriptor::ObjectDescriptor;
use tidepool_heap::static_region::StaticRegion;

/// The same code may have several instances with distinct closure identities.
/// Machines hold their own root blocks and native environments for this owner.
pub(crate) struct InstanceImage {
    pub(crate) descriptors: Vec<Arc<ObjectDescriptor>>,
    pub(crate) descriptor_registry: BTreeMap<usize, DescriptorMetadata>,
    pub(crate) statics: Arc<StaticRegion>,
    pub(crate) descriptor_words: Box<[usize]>,
    pub(crate) heap_top_specs: Vec<HeapTopSpec>,
    headers: BTreeMap<usize, usize>,
}

impl InstanceImage {
    pub(crate) fn new(code: &CompiledProgram) -> Result<Arc<Self>, ExecutionError> {
        let shared: HashSet<_> = code
            .interned_constructors
            .iter()
            .map(|(_, descriptor)| descriptor.initial_header_word())
            .chain(code.externals.headers())
            .collect();
        let mut replacements = BTreeMap::new();
        let descriptors: Vec<_> = code
            .descriptors
            .iter()
            .map(|descriptor| {
                let header = descriptor.initial_header_word();
                if shared.contains(&header) {
                    Arc::clone(descriptor)
                } else {
                    let replacement = Arc::new((**descriptor).clone());
                    replacements.insert(header, Arc::clone(&replacement));
                    replacement
                }
            })
            .collect();
        let headers: BTreeMap<_, _> = code
            .descriptors
            .iter()
            .zip(&descriptors)
            .map(|(original, replacement)| {
                (
                    original.initial_header_word(),
                    replacement.initial_header_word(),
                )
            })
            .collect();
        let descriptor_registry = code
            .descriptor_registry
            .iter()
            .map(|(&header, metadata)| {
                let replacement = replacements.get(&header).unwrap_or(&metadata.descriptor);
                (
                    headers[&header],
                    DescriptorMetadata {
                        descriptor: Arc::clone(replacement),
                        meaning: metadata.meaning.clone(),
                    },
                )
            })
            .collect();
        let descriptor_words = code
            .descriptor_slots
            .keys()
            .map(|header| headers[header])
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let statics = Arc::new(code.statics.instantiate_with_descriptors(&replacements)?);
        let heap_top_specs = code
            .heap_top_specs
            .iter()
            .map(|spec| HeapTopSpec {
                descriptor: replacements
                    .get(&spec.descriptor.initial_header_word())
                    .cloned()
                    .unwrap_or_else(|| Arc::clone(&spec.descriptor)),
                ..spec.clone()
            })
            .collect();
        Ok(Arc::new(Self {
            descriptors,
            descriptor_registry,
            statics,
            descriptor_words,
            heap_top_specs,
            headers,
        }))
    }

    pub(crate) fn header(&self, template: usize) -> usize {
        self.headers[&template]
    }
}
