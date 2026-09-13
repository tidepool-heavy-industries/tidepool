use std::io::Cursor;

use ciborium::value::Value;

use super::{
    Alternative, AlternativePattern, Architecture, Atom, CaseKind, CheckedLayout, ConstructorDecl,
    ConstructorId, DecodeLimits, Endianness, Expr, FieldLayout, GlobalDecl, GlobalId, Group,
    HeapBinding, HeapRhs, JoinBinding, JoinId, OperationDecl, OperationId, ParseError,
    ProgramEnvelope, RuntimeRep, ScalarLiteral, Signature, SignatureId, SymbolIdentity,
    TargetDescriptor, TopBinding, UpdatePolicy, ValueId, ValueRef, WireProgram,
};

/// Decode only the closed r7 CBOR grammar into an unpublished wire value.
/// Semantic validation and construction publication remain in `decode`.
pub(super) fn decode_wire(bytes: &[u8], limits: DecodeLimits) -> Result<WireProgram, ParseError> {
    if bytes.len() > limits.max_bytes {
        return Err(ParseError::ByteLimit {
            limit: limits.max_bytes,
            actual: bytes.len(),
        });
    }
    let consumed = scan_item(bytes, 0, 0, limits.max_depth)?;
    if consumed != bytes.len() {
        return Err(ParseError::TrailingBytes);
    }

    let mut cursor = Cursor::new(bytes);
    let value: Value = ciborium::de::from_reader(&mut cursor).map_err(|error| match error {
        ciborium::de::Error::Io(_) | ciborium::de::Error::Syntax(_) => ParseError::Truncated,
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
fn scan_item(
    bytes: &[u8],
    offset: usize,
    depth: usize,
    max_depth: usize,
) -> Result<usize, ParseError> {
    if depth > max_depth {
        return Err(ParseError::LimitExceeded("depth"));
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
    let mut next = offset
        .checked_add(head)
        .ok_or(ParseError::LimitExceeded("work"))?;
    match major {
        0 | 1 | 7 => Ok(next),
        2 | 3 => next
            .checked_add(usize::try_from(argument).map_err(|_| ParseError::LimitExceeded("work"))?)
            .filter(|end| *end <= bytes.len())
            .ok_or(ParseError::Truncated),
        4 => {
            for _ in 0..argument {
                next = scan_item(bytes, next, depth + 1, max_depth)?;
            }
            Ok(next)
        }
        5 => {
            for _ in 0..argument.saturating_mul(2) {
                next = scan_item(bytes, next, depth + 1, max_depth)?;
            }
            Ok(next)
        }
        6 => scan_item(bytes, next, depth + 1, max_depth),
        _ => Err(ParseError::Malformed("invalid CBOR major type".into())),
    }
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
        let fields = array(value, 12, "program")?;
        if text_raw(&fields[0], "program magic")? != "TPSTG" {
            return Err(ParseError::Malformed(
                "invalid prepared program magic".into(),
            ));
        }
        let target = self.target(&fields[5])?;
        let signatures = self.list(&fields[6], true, |this, value| this.signature(value))?;
        let globals = self.list(&fields[7], true, |this, value| this.global(value))?;
        let constructors = self.list(&fields[8], true, |this, value| this.constructor(value))?;
        let operations = self.list(&fields[9], true, |this, value| this.operation(value))?;
        let bindings = self.list(&fields[10], true, |this, value| this.top_group(value, 0))?;
        Ok(WireProgram {
            envelope: ProgramEnvelope {
                schema_version: unsigned(&fields[1], "schema version")?,
                projection_profile: self.text(&fields[2], "projection profile")?,
                toolchain: self.text(&fields[3], "toolchain")?,
                execution_abi_version: unsigned(&fields[4], "execution ABI version")?,
                target,
            },
            signatures,
            globals,
            constructors,
            operations,
            bindings,
            entry: ValueId(u32_value(&fields[11], "entry value ID")?),
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
        let fields = array(value, 4, "symbol")?;
        Ok(SymbolIdentity {
            unit: self.text(&fields[0], "symbol unit")?,
            module: self.text(&fields[1], "symbol module")?,
            namespace: self.text(&fields[2], "symbol namespace")?,
            occurrence: self.text(&fields[3], "symbol occurrence")?,
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
            results: self.list(&fields[1], false, |this, value| this.rep(value))?,
        })
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
        let fields = array(value, 8, "constructor declaration")?;
        Ok(ConstructorDecl {
            identity: self.symbol(&fields[0])?,
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
        Ok(OperationDecl {
            identity: self.text(&fields[0], "operation identity")?,
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
            (3, 2) => Ok(ScalarLiteral::Char(u32_value(
                &fields[1],
                "character codepoint",
            )?)),
            (4, 2) => Ok(ScalarLiteral::Bytes(
                self.bytes(&fields[1], "byte literal")?,
            )),
            (5, 1) => Ok(ScalarLiteral::NullAddress),
            (0..=5, _) => Err(ParseError::Malformed("wrong scalar field count".into())),
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

    fn heap_binding(&mut self, value: &Value, depth: usize) -> Result<HeapBinding, ParseError> {
        self.node()?;
        let fields = array(value, 2, "heap binding")?;
        Ok(HeapBinding {
            id: ValueId(u32_value(&fields[0], "heap value ID")?),
            rhs: self.heap_rhs(&fields[1], depth + 1)?,
        })
    }

    fn heap_rhs(&mut self, value: &Value, depth: usize) -> Result<HeapRhs, ParseError> {
        self.depth(depth)?;
        let fields = tagged(value, "heap RHS")?;
        let tag = unsigned(&fields[0], "heap RHS tag")?;
        match (tag, fields.len()) {
            (0, 5) => Ok(HeapRhs::Function {
                signature: SignatureId(u32_value(&fields[1], "function signature ID")?),
                parameters: self.list(&fields[2], false, |_this, value| {
                    Ok(ValueId(u32_value(value, "parameter ID")?))
                })?,
                captures: self.list(&fields[3], false, |this, value| this.value_ref(value))?,
                body: Box::new(self.expr(&fields[4], depth + 1)?),
            }),
            (1, 5) => Ok(HeapRhs::Thunk {
                signature: SignatureId(u32_value(&fields[1], "thunk signature ID")?),
                update: match unsigned(&fields[2], "update policy")? {
                    0 => UpdatePolicy::Memoize,
                    1 => UpdatePolicy::SingleEntry,
                    tag => return Err(ParseError::InvalidTag(tag)),
                },
                captures: self.list(&fields[3], false, |this, value| this.value_ref(value))?,
                body: Box::new(self.expr(&fields[4], depth + 1)?),
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

    fn join_binding(&mut self, value: &Value, depth: usize) -> Result<JoinBinding, ParseError> {
        self.node()?;
        let fields = array(value, 4, "join binding")?;
        Ok(JoinBinding {
            id: JoinId(u32_value(&fields[0], "join ID")?),
            signature: SignatureId(u32_value(&fields[1], "join signature ID")?),
            parameters: self.list(&fields[2], false, |_this, value| {
                Ok(ValueId(u32_value(value, "join parameter ID")?))
            })?,
            body: Box::new(self.expr(&fields[3], depth + 1)?),
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

    fn alternative(&mut self, value: &Value, depth: usize) -> Result<Alternative, ParseError> {
        self.node()?;
        let fields = array(value, 3, "alternative")?;
        Ok(Alternative {
            pattern: self.pattern(&fields[0])?,
            binders: self.list(&fields[1], false, |_this, value| {
                Ok(ValueId(u32_value(value, "alternative binder ID")?))
            })?,
            body: self.expr(&fields[2], depth + 1)?,
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

    fn expr(&mut self, value: &Value, depth: usize) -> Result<Expr, ParseError> {
        self.depth(depth)?;
        self.node()?;
        let fields = tagged(value, "expression")?;
        let tag = unsigned(&fields[0], "expression tag")?;
        match (tag, fields.len()) {
            (0, 2) => Ok(Expr::Return(self.list(
                &fields[1],
                false,
                |this, value| this.atom(value),
            )?)),
            (1, 3) => Ok(Expr::Enter {
                callee: self.atom(&fields[1])?,
                signature: SignatureId(u32_value(&fields[2], "enter signature ID")?),
            }),
            (2, 4) => Ok(Expr::Call {
                callee: self.atom(&fields[1])?,
                signature: SignatureId(u32_value(&fields[2], "call signature ID")?),
                arguments: self.list(&fields[3], false, |this, value| this.atom(value))?,
            }),
            (3, 3) => Ok(Expr::Operation {
                operation: OperationId(u32_value(&fields[1], "operation ID")?),
                arguments: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (4, 3) => Ok(Expr::Construct {
                constructor: ConstructorId(u32_value(&fields[1], "constructor ID")?),
                fields: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (5, 6) => Ok(Expr::Case {
                scrutinee: Box::new(self.expr(&fields[1], depth + 1)?),
                binder: ValueId(u32_value(&fields[2], "case binder ID")?),
                scrutinee_reps: self.list(&fields[3], false, |this, value| this.rep(value))?,
                kind: self.case_kind(&fields[4])?,
                alternatives: self.list(&fields[5], false, |this, value| {
                    this.alternative(value, depth + 1)
                })?,
            }),
            (6, 3) => Ok(Expr::Let {
                bindings: self.heap_group(&fields[1], depth + 1)?,
                body: Box::new(self.expr(&fields[2], depth + 1)?),
            }),
            (7, 3) => Ok(Expr::LetJoins {
                bindings: self.join_group(&fields[1], depth + 1)?,
                body: Box::new(self.expr(&fields[2], depth + 1)?),
            }),
            (8, 3) => Ok(Expr::Jump {
                join: JoinId(u32_value(&fields[1], "jump join ID")?),
                arguments: self.list(&fields[2], false, |this, value| this.atom(value))?,
            }),
            (0..=8, _) => Err(ParseError::Malformed("wrong expression field count".into())),
            _ => Err(ParseError::InvalidTag(tag)),
        }
    }

    fn heap_group(
        &mut self,
        value: &Value,
        depth: usize,
    ) -> Result<Group<HeapBinding>, ParseError> {
        self.group(value, |this, value| this.heap_binding(value, depth))
    }

    fn join_group(
        &mut self,
        value: &Value,
        depth: usize,
    ) -> Result<Group<JoinBinding>, ParseError> {
        self.group(value, |this, value| this.join_binding(value, depth))
    }

    fn top_group(&mut self, value: &Value, depth: usize) -> Result<Group<TopBinding>, ParseError> {
        self.group(value, |this, value| {
            this.node()?;
            let fields = array(value, 2, "top binding")?;
            Ok(TopBinding {
                identity: this.symbol(&fields[0])?,
                binding: this.heap_binding(&fields[1], depth + 1)?,
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

    fn depth(&self, depth: usize) -> Result<(), ParseError> {
        if depth > self.limits.max_depth {
            Err(ParseError::LimitExceeded("depth"))
        } else {
            Ok(())
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
