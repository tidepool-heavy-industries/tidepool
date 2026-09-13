use super::{plan::ProgramPlan, CompileError};
use tidepool_heap::static_region::StaticImage;

/// Reserve every top object before initializing any managed edge. Function
/// captures and constructor fields use the same descriptor logical layout as
/// generated allocation. Raw byte addresses point into ProgramPlan's pinned
/// bytes, never the input artifact. Bytes tops occupy top-table slots but are
/// not fake objects in the descriptor image.
pub(super) fn build_static_image(_plan: &ProgramPlan<'_>) -> Result<StaticImage, CompileError> {
    // wave4:STATIC_IMAGE — offsets for all Constructor/Function tops first;
    // descriptor headers + fields next, managed fields zero plus relocation;
    // hand to StaticImage::new to prove closure before publishing.
    todo!("wave4:STATIC_IMAGE")
}
