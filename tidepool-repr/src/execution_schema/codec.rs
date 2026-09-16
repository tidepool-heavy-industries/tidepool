use std::io::Cursor;

use ciborium::value::Value;

use super::{
    Alternative, AlternativePattern, Architecture, Atom, CaseKind, CheckedLayout, ConstructorDecl,
    ConstructorId, CtorRow, DecodeLimits, Endianness, Expr, ExprFrame, FieldLayout, GlobalDecl,
    GlobalId, Group, HeapBinding, HeapRhs, JoinBinding, JoinId, OperationDecl, OperationId,
    ParseError, ProgramEnvelope, RuntimeRep, ScalarLiteral, Signature, SignatureId, SiteDelivery,
    SiteRow, SymbolIdentity, TargetDescriptor, TopBinding, TypeNode, TypeNodeId, UpdatePolicy,
    ValueId, ValueRef, WireProgram,
};

// Flat schema records have bounded container nesting regardless of program
// depth. This is a malformed-wire guard, not an expression complexity limit.
const MAX_CONTAINER_NESTING: usize = 32;

/// Decode only the closed flat CBOR grammar into an unpublished wire value.
/// Semantic validation and construction publication remain in `decode`.
pub(super) fn decode_wire(bytes: &[u8], limits: DecodeLimits) -> Result<WireProgram, ParseError> {
    if bytes.len() > limits.max_bytes {
        return Err(ParseError::ByteLimit {
            limit: limits.max_bytes,
            actual: bytes.len(),
        });
    }
    let consumed = scan_item(bytes, limits.max_work)?;
    if consumed != bytes.len() {
        return Err(ParseError::TrailingBytes);
    }

    let mut cursor = Cursor::new(bytes);
    let value: Value =
        ciborium::de::from_reader_with_recursion_limit(&mut cursor, MAX_CONTAINER_NESTING)
            .map_err(|error| match error {
                ciborium::de::Error::Io(_) | ciborium::de::Error::Syntax(_) => {
                    ParseError::Truncated
                }
                ciborium::de::Error::RecursionLimitExceeded => ParseError::LimitExceeded("depth"),
                ciborium::de::Error::Semantic(_, detail) => ParseError::Malformed(detail),
            })?;
    if cursor.position() as usize != bytes.len() {
        return Err(ParseError::TrailingBytes);
    }
    Decoder::new(limits).program(&value)
}

/// Walk one complete CBOR item, rejecting indefinite containers before
/// `ciborium::Value` erases that distinction.
fn scan_item(bytes: &[u8], max_work: usize) -> Result<usize, ParseError> {
    let mut remaining = vec![1_u64];
    let mut offset = 0_usize;
    let mut work = 0_usize;
    while let Some(items) = remaining.last_mut() {
        if *items == 0 {
            remaining.pop();
            continue;
        }
        *items -= 1;
        work = work
            .checked_add(1)
            .ok_or(ParseError::LimitExceeded("work"))?;
        if work > max_work {
            return Err(ParseError::LimitExceeded("work"));
        }
        let initial = *bytes.get(offset).ok_or(ParseError::Truncated)?;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        if additional == 31 {
            return Err(ParseError::Malformed(
                "indefinite-length CBOR is not supported".into(),
            ));
        }
        let (argument, head) = cbor_argument(bytes, offset, additional)?;
        offset = offset
            .checked_add(head)
            .ok_or(ParseError::LimitExceeded("work"))?;
        let children = match major {
            0 | 1 | 7 => 0,
            2 | 3 => {
                offset = offset
                    .checked_add(
                        usize::try_from(argument).map_err(|_| ParseError::LimitExceeded("work"))?,
                    )
                    .filter(|end| *end <= bytes.len())
                    .ok_or(ParseError::Truncated)?;
                0
            }
            4 => argument,
            5 => argument
                .checked_mul(2)
                .ok_or(ParseError::LimitExceeded("work"))?,
            6 => 1,
            _ => return Err(ParseError::Malformed("invalid CBOR major type".into())),
        };
        if children != 0 {
            if remaining.len() >= MAX_CONTAINER_NESTING {
                return Err(ParseError::Malformed(
                    "flat schema container nesting exceeded".into(),
                ));
            }
            remaining.push(children);
        }
    }
    Ok(offset)
}

fn cbor_argument(bytes: &[u8], offset: usize, additional: u8) -> Result<(u64, usize), ParseError> {
    let read = |count: usize| {
        let start = offset + 1;
        let end = start.checked_add(count).ok_or(ParseError::Truncated)?;
        bytes.get(start..end).ok_or(ParseError::Truncated)
    };
    match additional {
        value @ 0..=23 => Ok((u64::from(value), 1)),
        24 => Ok((u64::from(read(1)?[0]), 2)),
        25 => Ok((
            u64::from(u16::from_be_bytes(
                read(2)?.try_into().map_err(|_| ParseError::Truncated)?,
            )),
            3,
        )),
        26 => Ok((
            u64::from(u32::from_be_bytes(
                read(4)?.try_into().map_err(|_| ParseError::Truncated)?,
            )),
            5,
        )),
        27 => Ok((
            u64::from_be_bytes(read(8)?.try_into().map_err(|_| ParseError::Truncated)?),
            9,
        )),
        _ => Err(ParseError::Malformed(
            "reserved CBOR additional information".into(),
        )),
    }
}

struct Decoder {
    limits: DecodeLimits,
    nodes: usize,
    tables: usize,
    strings: usize,
    work: usize,
}

impl Decoder {
    fn new(limits: DecodeLimits) -> Self {
        Self {
            limits,
            nodes: 0,
            tables: 0,
            strings: 0,
            work: 0,
        }
    }

    fn charge(&mut self, amount: usize) -> Result<(), ParseError> {
        self.work = self
            .work
            .checked_add(amount)
            .ok_or(ParseError::LimitExceeded("work"))?;
        if self.work > self.limits.max_work {
            return Err(ParseError::LimitExceeded("work"));
        }
        Ok(())
    }

    fn node(&mut self) -> Result<(), ParseError> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or(ParseError::LimitExceeded("nodes"))?;
        if self.nodes > self.limits.max_nodes {
            return Err(ParseError::LimitExceeded("nodes"));
        }
        self.charge(1)
    }

    fn table(&mut self, len: usize) -> Result<(), ParseError> {
        self.tables = self
            .tables
            .checked_add(len)
            .ok_or(ParseError::LimitExceeded("table entries"))?;
        if self.tables > self.limits.max_table_entries {
            return Err(ParseError::LimitExceeded("table entries"));
        }
        self.charge(len)
    }

    fn program(&mut self, value: &Value) -> Result<WireProgram, ParseError> {
        let Value::Array(header) = value else {
            return Err(malformed("program", "array"));
        };
        if header.len() < 2 {
            return Err(ParseError::Malformed("wrong program field count".into()));
        }
        if text_raw(&header[0], "program magic")? != "TPSTG" {
            return Err(ParseError::Malformed(
                "invalid prepared program magic".into(),
            ));
        }
        let schema_version = unsigned(&header[1], "schema version")?;
        if schema_version != super::SCHEMA_VERSION {
            return Err(ParseError::UnsupportedVersion(schema_version));
        }
        let fields = array(value, 16, "program")?;
        let target = self.target(&fields[5])?;
        let signatures = self.list(&fields[6], true, |this, value| this.signature(value))?;
        let globals = self.list(&fields[7], true, |this, value| this.global(value))?;
        let constructors = self.list(&fields[8], true, |this, value| this.constructor(value))?;
        let operations = self.list(&fields[9], true, |this, value| this.operation(value))?;
        let expressions = self.expr(&fields[10])?;
        let bindings = self.list(&fields[11], true, |this, value| this.top_group(value))?;
        let types = self.type_nodes(&fields[13])?;
        let sites = self.sites(&fields[14])?;
        let verb_sites = self.verb_sites(&fields[15])?;
        Ok(WireProgram {
            envelope: ProgramEnvelope {
                schema_version,
                projection_profile: self.text(&fields[2], "projection profile")?,
                toolchain: self.text(&fields[3], "toolchain")?,
                execution_abi_version: unsigned(&fields[4], "execution ABI version")?,
                target,
            },
            signatures,
            globals,
            constructors,
            operations,
            expressions,
            bindings,
            entry: ValueId(u32_value(&fields[12], "entry value ID")?),
            types,
            sites,
            verb_sites,
        })
    }

    fn type_nodes(&mut self, value: &Value) -> Result<Vec<TypeNode>, ParseError> {
        let Value::Array(values) = value else {
            return Err(malformed("type node table", "array"));
        };
        if values.len() > self.limits.max_type_nodes {
            return Err(ParseError::LimitExceeded("type nodes"));
        }
        self.list(value, false, |this, value| this.type_node(value))
    }

    fn sites(&mut self, value: &Value) -> Result<Vec<SiteRow>, ParseError> {
        let Value::Array(values) = value else {
            return Err(malformed("site table", "array"));
        };
        if values.len() > self.limits.max_sites {
            return Err(ParseError::LimitExceeded("sites"));
        }
        self.list(value, false, |this, value| this.site_row(value))
    }

    fn verb_sites(&mut self, value: &Value) -> Result<Vec<(ConstructorId, u64)>, ParseError> {
        let Value::Array(values) = value else {
            return Err(malformed("verb site table", "array"));
        };
        if values.len() > self.limits.max_sites {
            return Err(ParseError::LimitExceeded("verb sites"));
        }
        self.list(value, false, |_, value| {
            let fields = array(value, 2, "verb site")?;
            Ok((
                ConstructorId(u32_value(&fields[0], "verb site constructor ID")?),
                unsigned(&fields[1], "verb site ID")?,
            ))
        })
    }

    fn target(&mut self, value: &Value) -> Result<TargetDescriptor, ParseError> {
        let fields = array(value, 6, "target")?;
        let architecture = match unsigned(&fields[0], "architecture")? {
            0 => Architecture::X86_64,
            1 => Architecture::Aarch64,
            tag => return Err(ParseError::InvalidTag(tag)),
        };
        let endianness = match unsigned(&fields[1], "endianness")? {
            0 => Endianness::Little,
            1 => Endianness::Big,
            tag => return Err(ParseError::InvalidTag(tag)),
        };
        Ok(TargetDescriptor {
            architecture,
            endianness,
            pointer_width: u8_value(&fields[2], "pointer width")?,
            word_width: u8_value(&fields[3], "word width")?,
            abi: self.text(&fields[4], "target ABI")?,
            features: self.list(&fields[5], true, |this, value| {
                this.text(value, "target feature")
            })?,
        })
    }

    fn symbol(&mut self, value: &Value) -> Result<SymbolIdentity, ParseError> {
        let fields = array(value, 5, "symbol")?;
        let parent = tagged(&fields[4], "record parent")?;
        let parent_tag = unsigned(&parent[0], "record parent tag")?;
        let record_parent = match (parent_tag, parent.len()) {
            (0, 1) => None,
            (1, 2) => Some(self.text(&parent[1], "record parent")?),
            (0 | 1, _) => return Err(ParseError::Malformed("invalid record parent".into())),
            (tag, _) => return Err(ParseError::InvalidTag(tag)),
        };
        Ok(SymbolIdentity {
            unit: self.text(&fields[0], "symbol unit")?,
            module: self.text(&fields[1], "symbol module")?,
            namespace: self.text(&fields[2], "symbol namespace")?,
            occurrence: self.text(&fields[3], "symbol occurrence")?,
            record_parent,
        })
    }

    fn text(&mut self, value: &Value, what: &str) -> Result<String, ParseError> {
        let text = text_raw(value, what)?;
        self.strings = self
            .strings
            .checked_add(text.len())
            .ok_or(ParseError::LimitExceeded("string bytes"))?;
        if self.strings > self.limits.max_string_bytes {
            return Err(ParseError::LimitExceeded("string bytes"));
        }
        self.charge(text.len())?;
        Ok(text.to_owned())
    }

    fn bytes(&mut self, value: &Value, what: &str) -> Result<Vec<u8>, ParseError> {
        let Value::Bytes(bytes) = value else {
            return Err(malformed(what, "bytes"));
        };
        self.strings = self
            .strings
            .checked_add(bytes.len())
            .ok_or(ParseError::LimitExceeded("string bytes"))?;
        if self.strings > self.limits.max_string_bytes {
            return Err(ParseError::LimitExceeded("string bytes"));
        }
        self.charge(bytes.len())?;
        Ok(bytes.clone())
    }

    fn list<T>(
        &mut self,
        value: &Value,
        table: bool,
        mut decode: impl FnMut(&mut Self, &Value) -> Result<T, ParseError>,
    ) -> Result<Vec<T>, ParseError> {
        let Value::Array(values) = value else {
            return Err(malformed("list", "array"));
        };
        if table {
            self.table(values.len())?;
        } else {
            self.charge(values.len())?;
        }
        values.iter().map(|value| decode(self, value)).collect()
    }

    fn rep(&mut self, value: &Value) -> Result<RuntimeRep, ParseError> {
        let values = tagged(value, "runtime representation")?;
        let tag = unsigned(&values[0], "runtime representation tag")?;
        match (tag, values.len()) {
            (0, 1) => Ok(RuntimeRep::Void),
            (1, 1) => Ok(RuntimeRep::LiftedRef),
            (2, 1) => Ok(RuntimeRep::UnliftedRef),
            (3, 1) => Ok(RuntimeRep::Address),
            (4, 2) => Ok(RuntimeRep::Int(u8_value(&values[1], "integer bits")?)),
            (5, 2) => Ok(RuntimeRep::Word(u8_value(&values[1], "word bits")?)),
            (6, 2) => Ok(RuntimeRep::Float(u8_value(&values[1], "float bits")?)),
            (0..=6, _) => Err(ParseError::Malformed(
                "wrong runtime representation field count".into(),
            )),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn signature(&mut self, value: &Value) -> Result<Signature, ParseError> {
        let fields = array(value, 2, "signature")?;
        Ok(Signature {
            arguments: self.list(&fields[0], false, |this, value| this.rep(value))?,
            results: self.result_contract(&fields[1])?,
        })
    }

    fn result_contract(&mut self, value: &Value) -> Result<super::ResultContract, ParseError> {
        let fields = tagged(value, "result contract")?;
        match (unsigned(&fields[0], "result contract tag")?, fields.len()) {
            (0, 2) => Ok(super::ResultContract::Returns(self.list(
                &fields[1],
                false,
                |this, value| this.rep(value),
            )?)),
            (1, 1) => Ok(super::ResultContract::NoSuccess),
            (2, 1) => Ok(super::ResultContract::CallerResult),
            (0..=2, _) => Err(ParseError::Malformed(
                "wrong result contract field count".into(),
            )),
            (tag, _) => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn field_layout(&mut self, value: &Value) -> Result<FieldLayout, ParseError> {
        let fields = array(value, 2, "field layout")?;
        Ok(FieldLayout {
            rep: self.rep(&fields[0])?,
            offset: u32_value(&fields[1], "field offset")?,
        })
    }

    fn layout(&mut self, value: &Value) -> Result<CheckedLayout, ParseError> {
        let fields = array(value, 4, "checked layout")?;
        Ok(CheckedLayout {
            fields: self.list(&fields[0], false, |this, value| this.field_layout(value))?,
            alignment: u32_value(&fields[1], "layout alignment")?,
            payload_size: u32_value(&fields[2], "layout payload size")?,
            root_mask: self.list(&fields[3], false, |_this, value| {
                bool_value(value, "root mask")
            })?,
        })
    }

    fn constructor(&mut self, value: &Value) -> Result<ConstructorDecl, ParseError> {
        let fields = array(value, 9, "constructor declaration")?;
        Ok(ConstructorDecl {
            identity: self.symbol(&fields[0])?,
            host_id: crate::DataConId(unsigned(&fields[8], "constructor host ID")?),
            family: self.symbol(&fields[1])?,
            field_reps: self.list(&fields[2], false, |this, value| this.rep(value))?,
            strict_fields: self.list(&fields[3], false, |_this, value| {
                bool_value(value, "strict field")
            })?,
            layout: self.layout(&fields[4])?,
            result_rep: self.rep(&fields[5])?,
            tag: u32_value(&fields[6], "constructor tag")?,
            family_size: u32_value(&fields[7], "constructor family size")?,
        })
    }

    fn type_node(&mut self, value: &Value) -> Result<TypeNode, ParseError> {
        let fields = tagged(value, "type node")?;
        let tag = unsigned(&fields[0], "type node tag")?;
        match (tag, fields.len()) {
            (0, 4) => Ok(TypeNode::Data {
                family: self.symbol(&fields[1])?,
                arguments: self.list(&fields[2], false, |_, value| {
                    Ok(TypeNodeId(u32_value(value, "type argument node ID")?))
                })?,
                rows: self.list(&fields[3], false, |this, value| this.ctor_row(value))?,
            }),
            (1, 1) => Ok(TypeNode::Text),
            (2, 1) => Ok(TypeNode::Integer),
            (3, 1) => Ok(TypeNode::Natural),
            (4, 2) => Ok(TypeNode::Scalar(self.rep(&fields[1])?)),
            (5, 3) => Ok(TypeNode::Unconstructible {
                reason: self.text(&fields[1], "unconstructible reason")?,
                rendered: self.text(&fields[2], "unconstructible type")?,
            }),
            (0..=5, _) => Err(ParseError::Malformed("wrong type node field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn ctor_row(&mut self, value: &Value) -> Result<CtorRow, ParseError> {
        let fields = array(value, 2, "type constructor row")?;
        Ok(CtorRow {
            constructor: ConstructorId(u32_value(&fields[0], "type constructor ID")?),
            fields: self.list(&fields[1], false, |_, value| {
                Ok(TypeNodeId(u32_value(value, "type field node ID")?))
            })?,
        })
    }

    fn site_row(&mut self, value: &Value) -> Result<SiteRow, ParseError> {
        let fields = array(value, 6, "site row")?;
        let delivery = match unsigned(&fields[3], "site delivery")? {
            0 => SiteDelivery::HostAnswer,
            1 => SiteDelivery::LiveReentry,
            2 => SiteDelivery::ExitCellFill,
            3 => SiteDelivery::TerminalCapture,
            tag => return Err(ParseError::InvalidTag(tag)),
        };
        Ok(SiteRow {
            site: unsigned(&fields[0], "site ID")?,
            origin: self.text(&fields[1], "site origin")?,
            ordinal: unsigned(&fields[2], "site ordinal")?,
            delivery,
            wire: TypeNodeId(u32_value(&fields[4], "site wire node ID")?),
            inputs: self.list(&fields[5], false, |_, value| {
                Ok(TypeNodeId(u32_value(value, "site input node ID")?))
            })?,
        })
    }

    fn global(&mut self, value: &Value) -> Result<GlobalDecl, ParseError> {
        let fields = array(value, 5, "global declaration")?;
        Ok(GlobalDecl {
            identity: self.symbol(&fields[0])?,
            rep: self.rep(&fields[1])?,
            entry_signature: self.optional_signature(&fields[2])?,
            required_evaluated: bool_value(&fields[3], "required evaluated")?,
            required_generation: self.optional_generation(&fields[4])?,
        })
    }

    fn optional_signature(&mut self, value: &Value) -> Result<Option<SignatureId>, ParseError> {
        let fields = tagged(value, "known entry signature")?;
        match (
            unsigned(&fields[0], "known entry signature tag")?,
            fields.len(),
        ) {
            (0, 1) => Ok(None),
            (1, 2) => Ok(Some(SignatureId(u32_value(
                &fields[1],
                "entry signature ID",
            )?))),
            (0..=1, _) => Err(ParseError::Malformed(
                "wrong known entry signature field count".into(),
            )),
            (tag, _) => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn optional_generation(&mut self, value: &Value) -> Result<Option<u64>, ParseError> {
        let fields = tagged(value, "required generation")?;
        match (
            unsigned(&fields[0], "required generation tag")?,
            fields.len(),
        ) {
            (0, 1) => Ok(None),
            (1, 2) => Ok(Some(unsigned(&fields[1], "required generation")?)),
            (0..=1, _) => Err(ParseError::Malformed(
                "wrong required generation field count".into(),
            )),
            (tag, _) => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn operation(&mut self, value: &Value) -> Result<OperationDecl, ParseError> {
        let fields = array(value, 2, "operation declaration")?;
        let identity = tagged(&fields[0], "operation identity")?;
        let identity_tag = unsigned(&identity[0], "operation identity tag")?;
        let identity = match (identity_tag, identity.len()) {
            (0, 2) => super::OperationIdentity::PrimOp(self.text(&identity[1], "primop")?),
            (1, 3) => {
                let convention = tagged(&identity[2], "foreign convention")?;
                let convention_tag = unsigned(&convention[0], "foreign convention")?;
                match (convention_tag, convention.len()) {
                    (0, 1) => {}
                    (0, _) => {
                        return Err(ParseError::Malformed("invalid foreign convention".into()));
                    }
                    (tag, _) => return Err(ParseError::InvalidTag(tag)),
                }
                super::OperationIdentity::Intrinsic {
                    symbol: self.text(&identity[1], "intrinsic symbol")?,
                    convention: super::ForeignConvention::CCall,
                }
            }
            (2, 2) => super::OperationIdentity::Capability {
                name: self.text(&identity[1], "capability name")?,
            },
            (3, 2) => {
                let kind_tag = unsigned(&identity[1], "wired-in error kind")?;
                let kind = match kind_tag {
                    0 => super::WiredInErrorKind::PatternMatch,
                    1 => super::WiredInErrorKind::NonExhaustiveGuards,
                    2 => super::WiredInErrorKind::RecordSelector,
                    3 => super::WiredInErrorKind::RecordConstruction,
                    4 => super::WiredInErrorKind::NoMethodBinding,
                    5 => super::WiredInErrorKind::DeferredType,
                    6 => super::WiredInErrorKind::Impossible,
                    7 => super::WiredInErrorKind::ImpossibleConstraint,
                    8 => super::WiredInErrorKind::Absent,
                    9 => super::WiredInErrorKind::AbsentConstraint,
                    10 => super::WiredInErrorKind::AbsentSumField,
                    tag => return Err(ParseError::InvalidTag(tag)),
                };
                super::OperationIdentity::WiredInError { kind }
            }
            (0..=3, _) => {
                return Err(ParseError::Malformed("invalid operation identity".into()));
            }
            (tag, _) => return Err(ParseError::InvalidTag(tag)),
        };
        Ok(OperationDecl {
            identity,
            signature: SignatureId(u32_value(&fields[1], "operation signature ID")?),
        })
    }

    fn value_ref(&mut self, value: &Value) -> Result<ValueRef, ParseError> {
        let fields = tagged(value, "value reference")?;
        if fields.len() != 2 {
            return Err(ParseError::Malformed(
                "wrong value reference field count".into(),
            ));
        }
        match unsigned(&fields[0], "value reference tag")? {
            0 => Ok(ValueRef::Local(ValueId(u32_value(
                &fields[1],
                "local value ID",
            )?))),
            1 => Ok(ValueRef::Global(GlobalId(u32_value(
                &fields[1],
                "global ID",
            )?))),
            tag => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn scalar(&mut self, value: &Value) -> Result<ScalarLiteral, ParseError> {
        let fields = tagged(value, "scalar")?;
        let tag = unsigned(&fields[0], "scalar tag")?;
        match (tag, fields.len()) {
            (0, 3) => Ok(ScalarLiteral::Int {
                bits: u8_value(&fields[1], "integer bits")?,
                bytes: self.bytes(&fields[2], "integer payload")?,
            }),
            (1, 3) => Ok(ScalarLiteral::Word {
                bits: u8_value(&fields[1], "word bits")?,
                bytes: self.bytes(&fields[2], "word payload")?,
            }),
            (2, 3) => Ok(ScalarLiteral::Float {
                bits: u8_value(&fields[1], "float bits")?,
                bytes: self.bytes(&fields[2], "float payload")?,
            }),
            (4, 2) => Ok(ScalarLiteral::Bytes(
                self.bytes(&fields[1], "byte literal")?,
            )),
            (5, 1) => Ok(ScalarLiteral::NullAddress),
            (0..=2 | 4..=5, _) => Err(ParseError::Malformed("wrong scalar field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn atom(&mut self, value: &Value) -> Result<Atom, ParseError> {
        let fields = tagged(value, "atom")?;
        let tag = unsigned(&fields[0], "atom tag")?;
        match (tag, fields.len()) {
            (0, 2) => Ok(Atom::Ref(self.value_ref(&fields[1])?)),
            (1, 2) => Ok(Atom::Scalar(self.scalar(&fields[1])?)),
            (2, 1) => Ok(Atom::Void),
            (3, 2) => Ok(Atom::Rubbish(self.rep(&fields[1])?)),
            (0..=3, _) => Err(ParseError::Malformed("wrong atom field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn heap_binding<B>(
        &mut self,
        value: &Value,
        body: impl FnMut(&mut Self, &Value) -> Result<B, ParseError>,
    ) -> Result<HeapBinding<B>, ParseError> {
        self.node()?;
        let fields = array(value, 2, "heap binding")?;
        Ok(HeapBinding {
            id: ValueId(u32_value(&fields[0], "heap value ID")?),
            rhs: self.heap_rhs(&fields[1], body)?,
        })
    }

    fn heap_rhs<B>(
        &mut self,
        value: &Value,
        mut body: impl FnMut(&mut Self, &Value) -> Result<B, ParseError>,
    ) -> Result<HeapRhs<B>, ParseError> {
        let fields = tagged(value, "heap RHS")?;
        let tag = unsigned(&fields[0], "heap RHS tag")?;
        match (tag, fields.len()) {
            (0, 5) => Ok(HeapRhs::Function {
                signature: SignatureId(u32_value(&fields[1], "function signature ID")?),
                parameters: self.list(&fields[2], false, |_this, value| {
                    Ok(ValueId(u32_value(value, "parameter ID")?))
                })?,
                captures: self.list(&fields[3], false, |this, value| this.value_ref(value))?,
                body: body(self, &fields[4])?,
            }),
            (1, 5) => Ok(HeapRhs::Thunk {
                signature: SignatureId(u32_value(&fields[1], "thunk signature ID")?),
                update: match unsigned(&fields[2], "update policy")? {
                    0 => UpdatePolicy::Memoize,
                    1 => UpdatePolicy::SingleEntry,
                    tag => return Err(ParseError::InvalidTag(tag)),
                },
                captures: self.list(&fields[3], false, |this, value| this.value_ref(value))?,
                body: body(self, &fields[4])?,
            }),
            (2, 3) => Ok(HeapRhs::Constructor {
                constructor: ConstructorId(u32_value(&fields[1], "constructor ID")?),
                fields: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (3, 2) => Ok(HeapRhs::Bytes(self.bytes(&fields[1], "static bytes")?)),
            (0..=3, _) => Err(ParseError::Malformed("wrong heap RHS field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn join_binding(&mut self, value: &Value) -> Result<JoinBinding, ParseError> {
        self.node()?;
        let fields = array(value, 4, "join binding")?;
        Ok(JoinBinding {
            id: JoinId(u32_value(&fields[0], "join ID")?),
            signature: SignatureId(u32_value(&fields[1], "join signature ID")?),
            parameters: self.list(&fields[2], false, |_this, value| {
                Ok(ValueId(u32_value(value, "join parameter ID")?))
            })?,
            body: self.expr_index(&fields[3])?,
        })
    }

    fn pattern(&mut self, value: &Value) -> Result<AlternativePattern, ParseError> {
        let fields = tagged(value, "alternative pattern")?;
        let tag = unsigned(&fields[0], "pattern tag")?;
        match (tag, fields.len()) {
            (0, 1) => Ok(AlternativePattern::Default),
            (1, 2) => Ok(AlternativePattern::Constructor(ConstructorId(u32_value(
                &fields[1],
                "pattern constructor ID",
            )?))),
            (2, 2) => Ok(AlternativePattern::Literal(self.scalar(&fields[1])?)),
            (0..=2, _) => Err(ParseError::Malformed("wrong pattern field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn alternative(&mut self, value: &Value) -> Result<Alternative, ParseError> {
        self.node()?;
        let fields = array(value, 3, "alternative")?;
        Ok(Alternative {
            pattern: self.pattern(&fields[0])?,
            binders: self.list(&fields[1], false, |_this, value| {
                Ok(ValueId(u32_value(value, "alternative binder ID")?))
            })?,
            body: self.expr_index(&fields[2])?,
        })
    }

    fn case_kind(&mut self, value: &Value) -> Result<CaseKind, ParseError> {
        let fields = tagged(value, "case kind")?;
        let tag = unsigned(&fields[0], "case kind tag")?;
        match (tag, fields.len()) {
            (0, 2) => Ok(CaseKind::Algebraic(self.symbol(&fields[1])?)),
            (1, 2) => Ok(CaseKind::Primitive(self.rep(&fields[1])?)),
            (2, 1) => Ok(CaseKind::MultiValue),
            (3, 1) => Ok(CaseKind::Polymorphic),
            (0..=3, _) => Err(ParseError::Malformed("wrong case kind field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn expr(&mut self, value: &Value) -> Result<Expr, ParseError> {
        Ok(Expr {
            nodes: self.list(value, false, Self::expr_frame)?,
        })
    }

    fn expr_index(&mut self, value: &Value) -> Result<usize, ParseError> {
        usize::try_from(unsigned(value, "expression index")?)
            .map_err(|_| malformed("expression index", "usize"))
    }

    fn expr_frame(&mut self, value: &Value) -> Result<ExprFrame<usize>, ParseError> {
        self.node()?;
        let fields = tagged(value, "expression")?;
        let tag = unsigned(&fields[0], "expression tag")?;
        match (tag, fields.len()) {
            (0, 2) => Ok(ExprFrame::Return(self.list(
                &fields[1],
                false,
                |this, value| this.atom(value),
            )?)),
            (1, 3) => Ok(ExprFrame::Enter {
                callee: self.atom(&fields[1])?,
                signature: SignatureId(u32_value(&fields[2], "enter signature ID")?),
            }),
            (2, 4) => Ok(ExprFrame::Call {
                callee: self.atom(&fields[1])?,
                signature: SignatureId(u32_value(&fields[2], "call signature ID")?),
                arguments: self.list(&fields[3], false, |this, value| this.atom(value))?,
            }),
            (3, 3) => Ok(ExprFrame::Operation {
                operation: OperationId(u32_value(&fields[1], "operation ID")?),
                arguments: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (4, 3) => Ok(ExprFrame::Construct {
                constructor: ConstructorId(u32_value(&fields[1], "constructor ID")?),
                fields: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (5, 6) => Ok(ExprFrame::Case {
                scrutinee: self.expr_index(&fields[1])?,
                binder: ValueId(u32_value(&fields[2], "case binder ID")?),
                scrutinee_results: self.result_contract(&fields[3])?,
                kind: self.case_kind(&fields[4])?,
                alternatives: self
                    .list(&fields[5], false, |this, value| this.alternative(value))?,
            }),
            (6, 3) => Ok(ExprFrame::Let {
                bindings: self.heap_group(&fields[1])?,
                body: self.expr_index(&fields[2])?,
            }),
            (7, 3) => Ok(ExprFrame::LetJoins {
                bindings: self.join_group(&fields[1])?,
                body: self.expr_index(&fields[2])?,
            }),
            (8, 3) => Ok(ExprFrame::Jump {
                join: JoinId(u32_value(&fields[1], "jump join ID")?),
                arguments: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (0..=8, _) => Err(ParseError::Malformed("wrong expression field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn heap_group(&mut self, value: &Value) -> Result<Group<HeapBinding<usize>>, ParseError> {
        self.group(value, |this, value| {
            this.heap_binding(value, Self::expr_index)
        })
    }

    fn join_group(&mut self, value: &Value) -> Result<Group<JoinBinding>, ParseError> {
        self.group(value, |this, value| this.join_binding(value))
    }

    fn top_group(&mut self, value: &Value) -> Result<Group<TopBinding>, ParseError> {
        self.group(value, |this, value| {
            this.node()?;
            let fields = array(value, 2, "top binding")?;
            Ok(TopBinding {
                identity: this.symbol(&fields[0])?,
                binding: this.heap_binding(&fields[1], Self::expr_index)?,
            })
        })
    }

    fn group<T>(
        &mut self,
        value: &Value,
        mut decode: impl FnMut(&mut Self, &Value) -> Result<T, ParseError>,
    ) -> Result<Group<T>, ParseError> {
        let fields = tagged(value, "binding group")?;
        let tag = unsigned(&fields[0], "binding group tag")?;
        match (tag, fields.len()) {
            (0, 2) => Ok(Group::NonRecursive(decode(self, &fields[1])?)),
            (1, 2) => Ok(Group::Recursive(self.list(
                &fields[1],
                false,
                |this, value| decode(this, value),
            )?)),
            (0..=1, _) => Err(ParseError::Malformed(
                "wrong binding group field count".into(),
            )),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }
}

fn array<'a>(value: &'a Value, length: usize, what: &str) -> Result<&'a [Value], ParseError> {
    let Value::Array(values) = value else {
        return Err(malformed(what, "array"));
    };
    if values.len() != length {
        return Err(ParseError::Malformed(format!(
            "{what} has {} fields, expected {length}",
            values.len()
        )));
    }
    Ok(values)
}

fn tagged<'a>(value: &'a Value, what: &str) -> Result<&'a [Value], ParseError> {
    let Value::Array(values) = value else {
        return Err(malformed(what, "tagged array"));
    };
    if values.is_empty() {
        return Err(ParseError::Malformed(format!("{what} is empty")));
    }
    Ok(values)
}

fn unsigned(value: &Value, what: &str) -> Result<u64, ParseError> {
    let Value::Integer(integer) = value else {
        return Err(malformed(what, "unsigned integer"));
    };
    u64::try_from(*integer).map_err(|_| malformed(what, "unsigned integer"))
}

fn u32_value(value: &Value, what: &str) -> Result<u32, ParseError> {
    u32::try_from(unsigned(value, what)?).map_err(|_| malformed(what, "u32"))
}

fn u8_value(value: &Value, what: &str) -> Result<u8, ParseError> {
    u8::try_from(unsigned(value, what)?).map_err(|_| malformed(what, "u8"))
}

fn bool_value(value: &Value, what: &str) -> Result<bool, ParseError> {
    let Value::Bool(value) = value else {
        return Err(malformed(what, "boolean"));
    };
    Ok(*value)
}

fn text_raw<'a>(value: &'a Value, what: &str) -> Result<&'a str, ParseError> {
    let Value::Text(value) = value else {
        return Err(malformed(what, "text"));
    };
    Ok(value)
}

fn malformed(what: &str, expected: &str) -> ParseError {
    ParseError::Malformed(format!("{what} must be {expected}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deep_program(depth: usize) -> Vec<u8> {
        let array = Value::Array;
        let n = |value: usize| Value::Integer((value as u64).into());
        let text = |value: &str| Value::Text(value.into());
        let rep = || array(vec![n(4), n(64)]);
        let leaf = || {
            array(vec![
                n(0),
                array(vec![array(vec![
                    n(1),
                    array(vec![
                        n(0),
                        n(64),
                        Value::Bytes(1_i64.to_be_bytes().to_vec()),
                    ]),
                ])]),
            ])
        };
        let mut nodes = vec![leaf()];
        for level in 1..=depth {
            let body = nodes.len() - 1;
            let scrutinee = nodes.len();
            nodes.push(leaf());
            nodes.push(array(vec![
                n(5),
                n(scrutinee),
                n(level),
                array(vec![n(0), array(vec![rep()])]),
                array(vec![n(1), rep()]),
                array(vec![array(vec![array(vec![n(0)]), array(vec![]), n(body)])]),
            ]));
        }
        let root = nodes.len() - 1;
        let wire = array(vec![
            text("TPSTG"),
            n(super::super::SCHEMA_VERSION as usize),
            text("ghc-9.12-prepared-stg"),
            text("ghc-9.12.2"),
            n(super::super::EXECUTION_ABI_VERSION as usize),
            array(vec![
                n(0),
                n(0),
                n(64),
                n(64),
                text("sysv64"),
                array(vec![]),
            ]),
            array(vec![array(vec![
                array(vec![]),
                array(vec![n(0), array(vec![rep()])]),
            ])]),
            array(vec![]),
            array(vec![]),
            array(vec![]),
            array(nodes),
            array(vec![array(vec![
                n(0),
                array(vec![
                    array(vec![
                        text("deep"),
                        text("Fixture"),
                        text("value"),
                        text("entry"),
                        array(vec![n(0)]),
                    ]),
                    array(vec![
                        n(0),
                        array(vec![n(0), n(0), array(vec![]), array(vec![]), n(root)]),
                    ]),
                ]),
            ])]),
            n(0),
            array(vec![]),
            array(vec![]),
            array(vec![]),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&wire, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn deep_flat_program_is_stack_safe_through_decode_validation_and_drop() {
        const CHILD: &str = "TIDEPOOL_DEEP_FLAT_SCHEMA_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "execution_schema::codec::tests::deep_flat_program_is_stack_safe_through_decode_validation_and_drop", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let bytes = deep_program(20_000);
                let wire = decode_wire(&bytes, DecodeLimits::default()).unwrap();
                let requirements = super::super::ProgramRequirements {
                    schema_version: super::super::SCHEMA_VERSION,
                    projection_profile: wire.envelope.projection_profile.clone(),
                    toolchain: wire.envelope.toolchain.clone(),
                    execution_abi_version: super::super::EXECUTION_ABI_VERSION,
                    target: wire.envelope.target.clone(),
                };
                drop(wire);
                let prepared =
                    super::super::parse_program(&bytes, &requirements, DecodeLimits::default())
                        .unwrap();
                assert_eq!(prepared.expressions().nodes.len(), 40_001);
                let cloned = prepared.clone();
                assert_eq!(prepared, cloned);
                assert!(!format!("{prepared:?}").is_empty());
                drop((prepared, cloned));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn malformed_nested_cbor_is_rejected_before_recursive_materialization() {
        let mut bytes = vec![0x81; 100_000];
        bytes.push(0);
        assert!(matches!(
            decode_wire(&bytes, DecodeLimits::default()),
            Err(ParseError::Malformed(_))
        ));
        assert!(matches!(
            scan_item(&[0x9f, 0xff], 10),
            Err(ParseError::Malformed(_))
        ));
        assert!(matches!(
            scan_item(&[0x82, 0], 10),
            Err(ParseError::Truncated)
        ));
        assert!(matches!(
            scan_item(&[0x82, 0, 0], 2),
            Err(ParseError::LimitExceeded("work"))
        ));
    }

    fn number(value: u8) -> Value {
        Value::Integer(value.into())
    }

    #[test]
    fn null_address_and_managed_rubbish_have_distinct_wire_forms() {
        let mut decoder = Decoder::new(DecodeLimits::default());
        let null = Value::Array(vec![number(1), Value::Array(vec![number(5)])]);
        let rubbish = Value::Array(vec![number(3), Value::Array(vec![number(1)])]);
        assert_eq!(
            decoder.atom(&null).unwrap(),
            Atom::Scalar(ScalarLiteral::NullAddress)
        );
        assert_eq!(
            decoder.atom(&rubbish).unwrap(),
            Atom::Rubbish(RuntimeRep::LiftedRef)
        );
        // A literal pattern cannot smuggle an atom through the scalar grammar.
        assert!(decoder.scalar(&rubbish).is_err());
    }

    #[test]
    fn retired_character_scalar_tag_is_not_decoded() {
        let mut decoder = Decoder::new(DecodeLimits::default());
        assert!(matches!(
            decoder.scalar(&Value::Array(vec![number(3), number(65)])),
            Err(ParseError::InvalidTag(3))
        ));
    }

    #[test]
    fn rubbish_wire_representation_is_explicit_and_required() {
        let mut decoder = Decoder::new(DecodeLimits::default());
        for (wire_rep, rep) in [
            (vec![number(2)], RuntimeRep::UnliftedRef),
            (vec![number(3)], RuntimeRep::Address),
            (vec![number(4), number(64)], RuntimeRep::Int(64)),
            (vec![number(6), number(32)], RuntimeRep::Float(32)),
        ] {
            let atom = Value::Array(vec![number(3), Value::Array(wire_rep)]);
            assert_eq!(decoder.atom(&atom).unwrap(), Atom::Rubbish(rep));
        }
        assert!(decoder.atom(&Value::Array(vec![number(3)])).is_err());
        assert!(decoder
            .atom(&Value::Array(vec![number(3), number(1)]))
            .is_err());
    }
}
