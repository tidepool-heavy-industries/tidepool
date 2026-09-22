//! Machine-wide application targets. Descriptor ownership identifies the
//! program; full logical signatures establish application compatibility.
//! Generated callers borrow pinned signature metadata owned by their code.

use cranelift_module::FuncId;
use tidepool_repr::execution_schema::Signature;

pub(crate) struct CallableExport {
    pub header: usize,
    pub function: FuncId,
    pub signature: Signature,
}
