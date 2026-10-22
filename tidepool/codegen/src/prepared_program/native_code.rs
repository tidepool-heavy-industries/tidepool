//! Preparation preserves the selected immutable dependency graph separately
//! from the body image cache. Equal images need not have equal dependencies.

use std::collections::BTreeMap;
use std::sync::Arc;

use tidepool_repr::execution_schema::{SymbolIdentity, ValueId};
use tidepool_repr::native_code::{NativeCodeBody, NativeCodeRef};

use super::{
    CompileError, CompiledProgram, DemandedHomeImage, DemandedPackageImage, ExecutionError,
    ImageRegistry, LiteralOwner, NativeExportRequest, PackageLiteral, SourceBinder,
    SourceInstanceDomain, SourceLiteral,
};

/// An exact linked code reference and the image prepared from its own body.
/// This carries no source admission or permission to install a live instance.
pub struct PreparedNativeCode {
    code: NativeCodeRef,
    image: PreparedNativeImage,
}

enum PreparedNativeImage {
    Home(DemandedHomeImage),
    Package(Arc<DemandedPackageImage>),
}

impl PreparedNativeCode {
    pub fn prepare(
        code: NativeCodeRef,
        owners: &[Option<LiteralOwner>],
        packages: &BTreeMap<SymbolIdentity, PackageLiteral>,
        sources: &BTreeMap<SourceBinder, SourceLiteral>,
        registry: &ImageRegistry,
    ) -> Result<Self, CompileError> {
        let image = match code.body() {
            NativeCodeBody::Home(body) => PreparedNativeImage::Home(DemandedHomeImage::prepare(
                body.clone(),
                owners,
                packages,
                sources,
                registry,
            )?),
            NativeCodeBody::Package(body) => PreparedNativeImage::Package(Arc::new(
                DemandedPackageImage::prepare(body.clone(), owners, packages, sources, registry)?,
            )),
        };
        Ok(Self { code, image })
    }

    pub fn code(&self) -> &NativeCodeRef {
        &self.code
    }

    pub fn image(&self) -> &Arc<CompiledProgram> {
        match &self.image {
            PreparedNativeImage::Home(image) => image.image(),
            PreparedNativeImage::Package(image) => image.image(),
        }
    }

    pub fn request(
        self: &Arc<Self>,
        domain: SourceInstanceDomain,
        value: ValueId,
    ) -> Result<NativeExportRequest, ExecutionError> {
        NativeExportRequest::from_code(Arc::clone(self), domain, value)
    }

    pub(super) fn package_image(&self) -> Option<&Arc<DemandedPackageImage>> {
        match &self.image {
            PreparedNativeImage::Home(_) => None,
            PreparedNativeImage::Package(image) => Some(image),
        }
    }
}
