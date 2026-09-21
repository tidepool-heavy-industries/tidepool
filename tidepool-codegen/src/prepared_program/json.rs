//! Authenticated JSON intrinsics and their invocation-scoped managed sink.

use std::{collections::BTreeMap, sync::Arc};

use cranelift_codegen::ir::{self, types, AbiParam, InstBuilder, MemFlags, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};
use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use tidepool_heap::{
    execution_descriptor::ObjectDescriptor, external_storage::ExternalStorageKind,
};
use tidepool_repr::execution_schema::{
    JsonLayout, OperationIdentity, ResultContract, RuntimeRep, Signature,
};

use crate::{
    context::VMContext,
    descriptor_bridge::{marshal_descriptor_object, DescriptorValue},
    host_fns::{prepared_gc_trigger, RuntimeError},
    machine_state::MachineState,
    prepared_control::CallStatus,
};

use super::{roots::RootWords, CompiledProgram};

const NUMBER_TOKEN: &str = "$serde_json::private::Number";
pub(super) const PARSE_JSON_HOST: &str = "prepared_parse_json";

pub(super) fn recognize(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<(
    JsonLayout,
    tidepool_repr::execution_schema::ConstructorId,
    tidepool_repr::execution_schema::ConstructorId,
)> {
    let OperationIdentity::JsonDecode {
        layout,
        left,
        right,
    } = identity
    else {
        return None;
    };
    (signature.arguments
        == [
            RuntimeRep::UnliftedRef,
            RuntimeRep::Int(64),
            RuntimeRep::Int(64),
        ]
        && signature.results == ResultContract::Returns(vec![RuntimeRep::LiftedRef]))
    .then_some((*layout, *left, *right))
}

pub(super) fn emit_parse_json(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    descriptor: &ObjectDescriptor,
    layout: JsonLayout,
    left: tidepool_repr::execution_schema::ConstructorId,
    right: tidepool_repr::execution_schema::ConstructorId,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64); 7];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline
        .module
        .declare_function(PARSE_JSON_HOST, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let output = super::arrays::output_slot(builder);
    let layout_slot = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        20 * 4,
        2,
    ));
    for (index, constructor) in layout_ids(layout, left, right).into_iter().enumerate() {
        let value = builder.ins().iconst(types::I32, i64::from(constructor.0));
        builder
            .ins()
            .stack_store(value, layout_slot, (index * 4) as i32);
    }
    let layout = builder.ins().stack_addr(types::I64, layout_slot, 0);
    let owner = builder
        .ins()
        .iconst(types::I64, descriptor.initial_header_word() as i64);
    let call = builder.ins().call(
        host,
        &[
            vmctx,
            arguments[0],
            owner,
            arguments[1],
            arguments[2],
            layout,
            output,
        ],
    );
    super::arrays::finish_checked_call(builder, builder.inst_results(call)[0]);
    let result = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), output, 0);
    builder.declare_value_needs_stack_map(result);
    Ok(vec![result])
}

fn layout_ids(
    layout: JsonLayout,
    left: tidepool_repr::execution_schema::ConstructorId,
    right: tidepool_repr::execution_schema::ConstructorId,
) -> [tidepool_repr::execution_schema::ConstructorId; 20] {
    [
        layout.object,
        layout.array,
        layout.string,
        layout.number,
        layout.bool_,
        layout.null,
        layout.map_bin,
        layout.map_tip,
        layout.true_,
        layout.false_,
        layout.cons,
        layout.nil,
        layout.scientific,
        layout.integer_small,
        layout.integer_positive,
        layout.integer_negative,
        layout.text,
        layout.int,
        left,
        right,
    ]
}

fn int_bits(value: i64) -> [u8; 16] {
    (value as i128).to_ne_bytes()
}

#[derive(Clone, Copy)]
struct IntrinsicNode(usize);

#[derive(Clone, Copy)]
enum IntrinsicField {
    Node(IntrinsicNode),
    Bits([u8; 16]),
}

struct JsonDescriptors {
    object: Arc<ObjectDescriptor>,
    array: Arc<ObjectDescriptor>,
    string: Arc<ObjectDescriptor>,
    number: Arc<ObjectDescriptor>,
    bool_: Arc<ObjectDescriptor>,
    null: Arc<ObjectDescriptor>,
    bin: Arc<ObjectDescriptor>,
    tip: Arc<ObjectDescriptor>,
    true_: Arc<ObjectDescriptor>,
    false_: Arc<ObjectDescriptor>,
    cons: Arc<ObjectDescriptor>,
    nil: Arc<ObjectDescriptor>,
    scientific: Arc<ObjectDescriptor>,
    is: Arc<ObjectDescriptor>,
    ip: Arc<ObjectDescriptor>,
    in_: Arc<ObjectDescriptor>,
    text: Arc<ObjectDescriptor>,
    i_hash: Arc<ObjectDescriptor>,
    left: Arc<ObjectDescriptor>,
    right: Arc<ObjectDescriptor>,
}

impl JsonDescriptors {
    fn resolve(builder: &IntrinsicBuilder<'_>, layout: *const u32) -> Result<Self, RuntimeError> {
        if layout.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let ids = unsafe { std::slice::from_raw_parts(layout, 20) };
        let d = |index| builder.descriptor(ids[index]);
        let resolved = Self {
            object: d(0)?,
            array: d(1)?,
            string: d(2)?,
            number: d(3)?,
            bool_: d(4)?,
            null: d(5)?,
            bin: d(6)?,
            tip: d(7)?,
            true_: d(8)?,
            false_: d(9)?,
            cons: d(10)?,
            nil: d(11)?,
            scientific: d(12)?,
            is: d(13)?,
            ip: d(14)?,
            in_: d(15)?,
            text: d(16)?,
            i_hash: d(17)?,
            left: d(18)?,
            right: d(19)?,
        };
        let reps = |descriptor: &ObjectDescriptor| {
            descriptor
                .payload()
                .logical_to_stored()
                .iter()
                .map(|stored| {
                    stored
                        .and_then(|index| descriptor.payload().fields().get(index as usize))
                        .map_or(RuntimeRep::Void, |field| field.rep())
                })
                .collect::<Vec<_>>()
        };
        let lifted = RuntimeRep::LiftedRef;
        let scalar = RuntimeRep::Int(64);
        let unlifted = RuntimeRep::UnliftedRef;
        for descriptor in [
            &resolved.object,
            &resolved.array,
            &resolved.string,
            &resolved.number,
            &resolved.bool_,
            &resolved.left,
            &resolved.right,
        ] {
            if reps(descriptor) != [lifted] {
                return Err(RuntimeError::BadPointer);
            }
        }
        for descriptor in [
            &resolved.null,
            &resolved.tip,
            &resolved.true_,
            &resolved.false_,
            &resolved.nil,
        ] {
            if !reps(descriptor).is_empty() {
                return Err(RuntimeError::BadPointer);
            }
        }
        if reps(&resolved.cons) != [lifted, lifted]
            || reps(&resolved.text) != [unlifted, scalar, scalar]
            || reps(&resolved.i_hash) != [scalar]
            || reps(&resolved.is) != [scalar]
            || reps(&resolved.ip) != [unlifted]
            || reps(&resolved.in_) != [unlifted]
        {
            return Err(RuntimeError::BadPointer);
        }
        let scientific = reps(&resolved.scientific);
        if scientific != [lifted, scalar] && scientific != [lifted, lifted] {
            return Err(RuntimeError::BadPointer);
        }
        let bin = reps(&resolved.bin);
        if bin != [lifted, lifted, lifted, lifted, lifted]
            && bin != [scalar, lifted, lifted, lifted, lifted]
        {
            return Err(RuntimeError::BadPointer);
        }
        Ok(resolved)
    }
}

struct JsonSink<'a, 'b> {
    builder: &'a mut IntrinsicBuilder<'b>,
    d: JsonDescriptors,
    failure: Option<RuntimeError>,
}

impl JsonSink<'_, '_> {
    fn con(
        &mut self,
        descriptor: &ObjectDescriptor,
        fields: &[IntrinsicField],
    ) -> Result<IntrinsicNode, RuntimeError> {
        let result = self.builder.constructor(descriptor, fields);
        if let Err(error) = &result {
            self.failure.get_or_insert_with(|| error.clone());
        }
        result
    }

    fn text(&mut self, value: &str) -> Result<IntrinsicNode, RuntimeError> {
        let bytes = self.builder.bytes(value.as_bytes());
        if let Err(error) = &bytes {
            self.failure.get_or_insert_with(|| error.clone());
        }
        let bytes = bytes?;
        let descriptor = Arc::clone(&self.d.text);
        self.con(
            &descriptor,
            &[
                IntrinsicField::Node(bytes),
                IntrinsicField::Bits(int_bits(0)),
                IntrinsicField::Bits(int_bits(value.len() as i64)),
            ],
        )
    }

    fn integer(&mut self, coefficient: &str) -> Result<IntrinsicNode, RuntimeError> {
        if let Ok(value) = coefficient.parse::<i64>() {
            let descriptor = Arc::clone(&self.d.is);
            return self.con(&descriptor, &[IntrinsicField::Bits(int_bits(value))]);
        }
        let negative = coefficient.starts_with('-');
        let digits = coefficient.trim_start_matches('-').bytes();
        let mut limbs = vec![0_u64];
        for digit in digits {
            let mut carry = u64::from(digit - b'0');
            for limb in &mut limbs {
                let value = u128::from(*limb) * 10 + u128::from(carry);
                *limb = value as u64;
                carry = (value >> 64) as u64;
            }
            if carry != 0 {
                limbs.push(carry);
            }
        }
        while limbs.len() > 1 && limbs.last() == Some(&0) {
            limbs.pop();
        }
        let bytes = limbs
            .into_iter()
            .flat_map(u64::to_ne_bytes)
            .collect::<Vec<_>>();
        let payload = self.builder.bytes(&bytes);
        if let Err(error) = &payload {
            self.failure.get_or_insert_with(|| error.clone());
        }
        let payload = payload?;
        let descriptor = Arc::clone(if negative { &self.d.in_ } else { &self.d.ip });
        self.con(&descriptor, &[IntrinsicField::Node(payload)])
    }

    fn number(&mut self, token: &str) -> Result<IntrinsicNode, RuntimeError> {
        let (coefficient, exponent) = tidepool_bridge::decimal::Decimal::parse_token(token)
            .map_err(|_| RuntimeError::BadPointer)?
            .into_parts();
        let coefficient = self.integer(&coefficient)?;
        let scientific = Arc::clone(&self.d.scientific);
        let exponent = match scientific
            .payload()
            .logical_to_stored()
            .get(1)
            .and_then(|stored| *stored)
            .and_then(|stored| scientific.payload().fields().get(stored as usize))
            .map(|field| field.rep())
        {
            Some(RuntimeRep::Int(64)) => IntrinsicField::Bits(int_bits(exponent)),
            Some(RuntimeRep::LiftedRef) => {
                let boxed = Arc::clone(&self.d.i_hash);
                IntrinsicField::Node(self.con(&boxed, &[IntrinsicField::Bits(int_bits(exponent))])?)
            }
            _ => return Err(RuntimeError::BadPointer),
        };
        let scientific = self.con(&scientific, &[IntrinsicField::Node(coefficient), exponent])?;
        let number = Arc::clone(&self.d.number);
        self.con(&number, &[IntrinsicField::Node(scientific)])
    }

    fn list(&mut self, values: Vec<IntrinsicNode>) -> Result<IntrinsicNode, RuntimeError> {
        let nil = Arc::clone(&self.d.nil);
        let mut tail = self.con(&nil, &[])?;
        let cons = Arc::clone(&self.d.cons);
        for value in values.into_iter().rev() {
            tail = self.con(
                &cons,
                &[IntrinsicField::Node(value), IntrinsicField::Node(tail)],
            )?;
        }
        Ok(tail)
    }

    fn map(
        &mut self,
        entries: BTreeMap<String, IntrinsicNode>,
    ) -> Result<IntrinsicNode, RuntimeError> {
        let entries = entries.into_iter().collect::<Vec<_>>();
        self.map_slice(&entries)
    }

    fn map_slice(
        &mut self,
        entries: &[(String, IntrinsicNode)],
    ) -> Result<IntrinsicNode, RuntimeError> {
        if entries.is_empty() {
            let tip = Arc::clone(&self.d.tip);
            return self.con(&tip, &[]);
        }
        let mid = entries.len() / 2;
        let key = self.text(&entries[mid].0)?;
        let left = self.map_slice(&entries[..mid])?;
        let right = self.map_slice(&entries[mid + 1..])?;
        let bin = Arc::clone(&self.d.bin);
        let size = match bin
            .payload()
            .logical_to_stored()
            .first()
            .and_then(|stored| *stored)
            .and_then(|stored| bin.payload().fields().get(stored as usize))
            .map(|field| field.rep())
        {
            Some(RuntimeRep::Int(64)) => IntrinsicField::Bits(int_bits(entries.len() as i64)),
            Some(RuntimeRep::LiftedRef) => {
                let boxed = Arc::clone(&self.d.i_hash);
                IntrinsicField::Node(self.con(
                    &boxed,
                    &[IntrinsicField::Bits(int_bits(entries.len() as i64))],
                )?)
            }
            _ => return Err(RuntimeError::BadPointer),
        };
        self.con(
            &bin,
            &[
                size,
                IntrinsicField::Node(key),
                IntrinsicField::Node(entries[mid].1),
                IntrinsicField::Node(left),
                IntrinsicField::Node(right),
            ],
        )
    }
}

pub(super) unsafe extern "C" fn prepared_parse_json(
    vmctx: *mut VMContext,
    reference: *mut u8,
    descriptor: *const ObjectDescriptor,
    offset: i64,
    length: i64,
    layout: *const u32,
    output: *mut u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| {
        if output.is_null() {
            return Err(RuntimeError::BadPointer);
        }
        let (published, total) = unsafe {
            super::arrays::active_payload(
                machine,
                vmctx,
                reference,
                descriptor,
                ExternalStorageKind::Bytes,
            )
        }?;
        let offset = usize::try_from(offset).map_err(|_| RuntimeError::ArrayIndexOutOfBounds {
            index: offset,
            len: total,
        })?;
        let length = usize::try_from(length).map_err(|_| RuntimeError::ArrayIndexOutOfBounds {
            index: length,
            len: total,
        })?;
        let input = machine
            .read_external_payload_offset(published, offset, length)
            .map_err(super::byte_arrays::byte_range_error)?;
        let input = std::str::from_utf8(&input).map_err(|_| RuntimeError::BadPointer)?;
        let mut builder = unsafe { IntrinsicBuilder::active(machine, &mut *vmctx) }?;
        let descriptors = JsonDescriptors::resolve(&builder, layout)?;
        let mut sink = JsonSink {
            builder: &mut builder,
            d: descriptors,
            failure: None,
        };
        let mut deserializer = serde_json::Deserializer::from_str(input);
        let parsed = JsonSeed(&mut sink)
            .deserialize(&mut deserializer)
            .and_then(|node| deserializer.end().map(|()| node));
        if let Some(error) = sink.failure.take() {
            return Err(error);
        }
        let result = match parsed {
            Ok(value) => {
                let right = Arc::clone(&sink.d.right);
                sink.con(&right, &[IntrinsicField::Node(value)])?
            }
            Err(error) => {
                let message = sink.text(&error.to_string())?;
                let left = Arc::clone(&sink.d.left);
                sink.con(&left, &[IntrinsicField::Node(message)])?
            }
        };
        let word = sink.builder.word(result)?;
        unsafe { output.write(word as u64) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(error) => super::arrays::array_error(machine, error),
    }
}

struct JsonSeed<'seed, 'builder, 'machine>(&'seed mut JsonSink<'builder, 'machine>);

impl<'de> DeserializeSeed<'de> for JsonSeed<'_, '_, '_> {
    type Value = IntrinsicNode;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for JsonSeed<'_, '_, '_> {
    type Value = IntrinsicNode;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let d = Arc::clone(&self.0.d.null);
        self.0.con(&d, &[]).map_err(E::custom)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let payload = Arc::clone(if value {
            &self.0.d.true_
        } else {
            &self.0.d.false_
        });
        let payload = self.0.con(&payload, &[]).map_err(E::custom)?;
        let d = Arc::clone(&self.0.d.bool_);
        self.0
            .con(&d, &[IntrinsicField::Node(payload)])
            .map_err(E::custom)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let text = self.0.text(value).map_err(E::custom)?;
        let d = Arc::clone(&self.0.d.string);
        self.0
            .con(&d, &[IntrinsicField::Node(text)])
            .map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_str(&value)
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.0.number(&value.to_string()).map_err(E::custom)
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.0.number(&value.to_string()).map_err(E::custom)
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.0.number(&value.to_string()).map_err(E::custom)
    }

    fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = access.next_element_seed(JsonSeed(self.0))? {
            values.push(value);
        }
        let list = self.0.list(values).map_err(A::Error::custom)?;
        let d = Arc::clone(&self.0.d.array);
        self.0
            .con(&d, &[IntrinsicField::Node(list)])
            .map_err(A::Error::custom)
    }

    fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let Some(first) = access.next_key::<String>()? else {
            let map = self.0.map(BTreeMap::new()).map_err(A::Error::custom)?;
            let d = Arc::clone(&self.0.d.object);
            return self
                .0
                .con(&d, &[IntrinsicField::Node(map)])
                .map_err(A::Error::custom);
        };
        if first == NUMBER_TOKEN {
            let token = access.next_value::<String>()?;
            return self.0.number(&token).map_err(A::Error::custom);
        }
        let mut entries = BTreeMap::new();
        entries.insert(first, access.next_value_seed(JsonSeed(self.0))?);
        while let Some(key) = access.next_key::<String>()? {
            entries.insert(key, access.next_value_seed(JsonSeed(self.0))?);
        }
        let map = self.0.map(entries).map_err(A::Error::custom)?;
        let d = Arc::clone(&self.0.d.object);
        self.0
            .con(&d, &[IntrinsicField::Node(map)])
            .map_err(A::Error::custom)
    }
}

struct IntrinsicBuilder<'a> {
    machine: &'a MachineState,
    vmctx: &'a mut VMContext,
    program: &'a CompiledProgram,
    roots: Vec<RootWords>,
    roots_mark: usize,
}

impl Drop for IntrinsicBuilder<'_> {
    fn drop(&mut self) {
        self.machine.truncate_rust_roots(self.roots_mark);
    }
}

impl<'a> IntrinsicBuilder<'a> {
    /// # Safety
    /// `vmctx` belongs to `machine`; the invocation scope keeps the active
    /// program and prepared old-space owner fixed until this builder drops.
    unsafe fn active(
        machine: &'a MachineState,
        vmctx: &'a mut VMContext,
    ) -> Result<Self, RuntimeError> {
        let program =
            unsafe { machine.active_intrinsic_program() }.ok_or(RuntimeError::BadPointer)?;
        Ok(Self {
            machine,
            vmctx,
            program,
            roots: Vec::new(),
            roots_mark: machine.rust_roots_len(),
        })
    }

    fn descriptor(&self, index: u32) -> Result<Arc<ObjectDescriptor>, RuntimeError> {
        self.program
            .interned_constructors
            .get(index as usize)
            .map(|(_, descriptor)| Arc::clone(descriptor))
            .ok_or(RuntimeError::BadPointer)
    }

    fn ensure_capacity(&mut self, extent: usize) -> Result<(), RuntimeError> {
        let free = (self.vmctx.alloc_limit as usize).saturating_sub(self.vmctx.alloc_ptr as usize);
        if free >= extent {
            return Ok(());
        }
        let status = unsafe { prepared_gc_trigger(self.vmctx, extent) };
        let status =
            CallStatus::from_raw(i64::from(status)).map_err(|_| RuntimeError::BadPointer)?;
        if status != CallStatus::Success
            || self.machine.prepared_call_status() != CallStatus::Success
        {
            return Err(RuntimeError::BadPointer);
        }
        let free = (self.vmctx.alloc_limit as usize).saturating_sub(self.vmctx.alloc_ptr as usize);
        (free >= extent)
            .then_some(())
            .ok_or(RuntimeError::HeapOverflow)
    }

    fn push_root(&mut self, word: usize) -> Result<IntrinsicNode, RuntimeError> {
        let root = RootWords::new(1).map_err(|_| RuntimeError::HeapOverflow)?;
        root.write(0, word as u64)
            .map_err(|_| RuntimeError::BadPointer)?;
        let slot = root.slot_address(0).ok_or(RuntimeError::BadPointer)?;
        self.machine.register_rust_root(slot);
        self.roots.push(root);
        Ok(IntrinsicNode(self.roots.len() - 1))
    }

    fn word(&self, node: IntrinsicNode) -> Result<usize, RuntimeError> {
        self.roots
            .get(node.0)
            .ok_or(RuntimeError::BadPointer)?
            .read(0)
            .map(|word| word as usize)
    }

    fn constructor(
        &mut self,
        descriptor: &ObjectDescriptor,
        fields: &[IntrinsicField],
    ) -> Result<IntrinsicNode, RuntimeError> {
        let extent = (descriptor.allocation_extent() as usize).next_multiple_of(8);
        self.ensure_capacity(extent)?;
        // Resolve rooted children only after the last possible collection.
        let values = fields
            .iter()
            .map(|field| match field {
                IntrinsicField::Node(node) => self
                    .word(*node)
                    .map(|word| DescriptorValue::Managed(word as *mut u8)),
                IntrinsicField::Bits(bits) => Ok(DescriptorValue::Bits(*bits)),
            })
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        let pointer = self.vmctx.alloc_ptr;
        unsafe { marshal_descriptor_object(pointer, extent, descriptor, &values) }
            .map_err(|_| RuntimeError::BadPointer)?;
        self.vmctx.alloc_ptr = unsafe { pointer.add(extent) };
        self.push_root(pointer as usize | usize::from(descriptor.tag()))
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<IntrinsicNode, RuntimeError> {
        let descriptor = Arc::clone(&self.program.externals.bytes_array);
        let extent = (descriptor.allocation_extent() as usize).next_multiple_of(8);
        self.ensure_capacity(extent)?;
        let payload = self
            .machine
            .allocate_external_storage(ExternalStorageKind::Bytes, bytes.len())
            .map_err(|_| RuntimeError::HeapOverflow)?;
        if self
            .machine
            .store_external_bytes(payload, 0, bytes)
            .is_err()
        {
            self.machine.release_external_storage(payload);
            return Err(RuntimeError::BadPointer);
        }
        let pointer = self.vmctx.alloc_ptr;
        if unsafe {
            marshal_descriptor_object(
                pointer,
                extent,
                &descriptor,
                &[DescriptorValue::Address(payload.cast_const())],
            )
        }
        .is_err()
        {
            self.machine.release_external_storage(payload);
            return Err(RuntimeError::BadPointer);
        }
        self.vmctx.alloc_ptr = unsafe { pointer.add(extent) };
        self.push_root(pointer as usize | usize::from(descriptor.tag()))
    }
}
