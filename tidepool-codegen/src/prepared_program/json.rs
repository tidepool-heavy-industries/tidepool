//! Authenticated JSON intrinsics and their invocation-scoped managed sink.

use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    sync::Arc,
};

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
use tidepool_repr::DataConId;

use crate::{
    context::VMContext,
    descriptor_bridge::DescriptorValue,
    host_fns::{prepared_gc_trigger, RuntimeError},
    machine_state::MachineState,
    prepared_control::CallStatus,
};

use super::{
    construction::{ConstructionCore, ConstructionError, ConstructionNode},
    CompiledProgram,
};

const NUMBER_TOKEN: &str = "$serde_json::private::Number";
const MAX_JSON_VALUE_DEPTH: usize = 128;
pub(super) const PARSE_JSON_HOST: &str = "prepared_parse_json";
pub(super) const ENCODE_JSON_HOST: &str = "prepared_encode_json";

/// The JIT-facing form of the one authenticated JSON payload layout. Decode
/// result constructors are deliberately not part of this object.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct JsonLayoutIds {
    object: u32,
    array: u32,
    string: u32,
    number: u32,
    bool_: u32,
    null: u32,
    map_bin: u32,
    map_tip: u32,
    true_: u32,
    false_: u32,
    cons: u32,
    nil: u32,
    scientific: u32,
    integer_small: u32,
    integer_positive: u32,
    integer_negative: u32,
    text: u32,
    int: u32,
}

impl From<JsonLayout> for JsonLayoutIds {
    fn from(layout: JsonLayout) -> Self {
        Self {
            object: layout.object.0,
            array: layout.array.0,
            string: layout.string.0,
            number: layout.number.0,
            bool_: layout.bool_.0,
            null: layout.null.0,
            map_bin: layout.map_bin.0,
            map_tip: layout.map_tip.0,
            true_: layout.true_.0,
            false_: layout.false_.0,
            cons: layout.cons.0,
            nil: layout.nil.0,
            scientific: layout.scientific.0,
            integer_small: layout.integer_small.0,
            integer_positive: layout.integer_positive.0,
            integer_negative: layout.integer_negative.0,
            text: layout.text.0,
            int: layout.int.0,
        }
    }
}

fn emit_layout_ids(builder: &mut FunctionBuilder<'_>, slot: ir::StackSlot, layout: JsonLayout) {
    let ids = JsonLayoutIds::from(layout);
    macro_rules! store {
        ($field:ident) => {{
            let value = builder.ins().iconst(types::I32, i64::from(ids.$field));
            builder.ins().stack_store(
                value,
                slot,
                std::mem::offset_of!(JsonLayoutIds, $field) as i32,
            );
        }};
    }
    store!(object);
    store!(array);
    store!(string);
    store!(number);
    store!(bool_);
    store!(null);
    store!(map_bin);
    store!(map_tip);
    store!(true_);
    store!(false_);
    store!(cons);
    store!(nil);
    store!(scientific);
    store!(integer_small);
    store!(integer_positive);
    store!(integer_negative);
    store!(text);
    store!(int);
}

pub(super) fn recognize(
    identity: &OperationIdentity,
    signature: &Signature,
) -> Option<(
    tidepool_repr::execution_schema::ConstructorId,
    tidepool_repr::execution_schema::ConstructorId,
)> {
    let OperationIdentity::JsonDecode { left, right } = identity else {
        return None;
    };
    (signature.arguments
        == [
            RuntimeRep::UnliftedRef,
            RuntimeRep::Int(64),
            RuntimeRep::Int(64),
        ]
        && signature.results == ResultContract::Returns(vec![RuntimeRep::LiftedRef]))
    .then_some((*left, *right))
}

pub(super) fn recognize_encode(identity: &OperationIdentity, signature: &Signature) -> bool {
    let OperationIdentity::JsonEncode = identity else {
        return false;
    };
    signature.arguments == [RuntimeRep::LiftedRef]
        && signature.results == ResultContract::Returns(vec![RuntimeRep::LiftedRef])
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
    signature.params = vec![AbiParam::new(types::I64); 9];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline
        .module
        .declare_function(PARSE_JSON_HOST, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let output = super::arrays::output_slot(builder);
    let layout_slot = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        std::mem::size_of::<JsonLayoutIds>() as u32,
        2,
    ));
    emit_layout_ids(builder, layout_slot, layout);
    let layout = builder.ins().stack_addr(types::I64, layout_slot, 0);
    let left = builder.ins().iconst(types::I64, i64::from(left.0));
    let right = builder.ins().iconst(types::I64, i64::from(right.0));
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
            left,
            right,
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

pub(super) fn emit_encode_json(
    builder: &mut FunctionBuilder<'_>,
    pipeline: &mut crate::pipeline::CodegenPipeline,
    vmctx: Value,
    layout: JsonLayout,
    arguments: &[Value],
) -> Result<Vec<Value>, super::CompileError> {
    let mut signature = ir::Signature::new(pipeline.isa.default_call_conv());
    signature.params = vec![AbiParam::new(types::I64); 4];
    signature.returns = vec![AbiParam::new(types::I32)];
    let host = pipeline
        .module
        .declare_function(ENCODE_JSON_HOST, Linkage::Import, &signature)
        .map_err(|error| crate::pipeline::PipelineError::Declaration(error.to_string()))?;
    let host = pipeline.module.declare_func_in_func(host, builder.func);
    let layout_slot = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        std::mem::size_of::<JsonLayoutIds>() as u32,
        2,
    ));
    emit_layout_ids(builder, layout_slot, layout);
    let layout = builder.ins().stack_addr(types::I64, layout_slot, 0);
    let output = super::arrays::output_slot(builder);
    builder.declare_value_needs_stack_map(arguments[0]);
    let call = builder
        .ins()
        .call(host, &[vmctx, arguments[0], layout, output]);
    super::arrays::finish_checked_call(builder, builder.inst_results(call)[0]);
    let result = builder
        .ins()
        .load(types::I64, MemFlags::trusted(), output, 0);
    builder.declare_value_needs_stack_map(result);
    Ok(vec![result])
}

fn int_bits(value: i64) -> [u8; 16] {
    (value as i128).to_ne_bytes()
}

type IntrinsicNode = ConstructionNode;

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
}

impl JsonDescriptors {
    fn resolve(
        builder: &IntrinsicBuilder<'_>,
        layout: *const JsonLayoutIds,
    ) -> Result<Self, RuntimeError> {
        let ids = unsafe { layout.as_ref() }.ok_or(RuntimeError::BadPointer)?;
        let d = |id| builder.descriptor(id);
        let resolved = Self {
            object: d(ids.object)?,
            array: d(ids.array)?,
            string: d(ids.string)?,
            number: d(ids.number)?,
            bool_: d(ids.bool_)?,
            null: d(ids.null)?,
            bin: d(ids.map_bin)?,
            tip: d(ids.map_tip)?,
            true_: d(ids.true_)?,
            false_: d(ids.false_)?,
            cons: d(ids.cons)?,
            nil: d(ids.nil)?,
            scientific: d(ids.scientific)?,
            is: d(ids.integer_small)?,
            ip: d(ids.integer_positive)?,
            in_: d(ids.integer_negative)?,
            text: d(ids.text)?,
            i_hash: d(ids.int)?,
        };
        let reps = descriptor_reps;
        let lifted = RuntimeRep::LiftedRef;
        let scalar = RuntimeRep::Int(64);
        let unlifted = RuntimeRep::UnliftedRef;
        for descriptor in [
            &resolved.object,
            &resolved.array,
            &resolved.string,
            &resolved.number,
            &resolved.bool_,
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
        if scientific != [lifted, scalar] {
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

fn descriptor_reps(descriptor: &ObjectDescriptor) -> Vec<RuntimeRep> {
    descriptor
        .payload()
        .logical_to_stored()
        .iter()
        .map(|stored| {
            stored
                .and_then(|index| descriptor.payload().fields().get(index as usize))
                .map_or(RuntimeRep::Void, |field| field.rep())
        })
        .collect()
}

struct JsonSink<'a, 'b> {
    builder: &'a mut IntrinsicBuilder<'b>,
    d: JsonDescriptors,
    failure: Option<RuntimeError>,
    left: Arc<ObjectDescriptor>,
    right: Arc<ObjectDescriptor>,
    input: std::ops::Range<usize>,
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

    fn release_node(&mut self, node: IntrinsicNode) -> Result<(), RuntimeError> {
        self.builder.release_node(node)
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
        let digits = coefficient.trim_start_matches('-').as_bytes();
        let mut limbs = vec![0_u64];
        for chunk in digits.chunks(19) {
            if self
                .builder
                .machine
                .poll_prepared(crate::prepared_control::PreparedSafepoint::Backedge)
                != CallStatus::Success
            {
                return Err(RuntimeError::Cancelled);
            }
            let factor = 10_u64.pow(chunk.len() as u32);
            let mut carry = std::str::from_utf8(chunk)
                .ok()
                .and_then(|digits| digits.parse::<u64>().ok())
                .ok_or(RuntimeError::BadPointer)?;
            for (index, limb) in limbs.iter_mut().enumerate() {
                if index % 1024 == 0
                    && self
                        .builder
                        .machine
                        .poll_prepared(crate::prepared_control::PreparedSafepoint::Backedge)
                        != CallStatus::Success
                {
                    return Err(RuntimeError::Cancelled);
                }
                let value = u128::from(*limb) * u128::from(factor) + u128::from(carry);
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
        let exponent = IntrinsicField::Bits(int_bits(exponent));
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
    layout: *const JsonLayoutIds,
    left: u64,
    right: u64,
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
        let input = machine.read_external_payload_offset_polling(published, offset, length)?;
        let input = std::str::from_utf8(&input).map_err(|_| RuntimeError::BadPointer)?;
        let input_start = input.as_ptr() as usize;
        let input_end = input_start
            .checked_add(input.len())
            .ok_or(RuntimeError::BadPointer)?;
        let input_range = input_start..input_end;
        let mut builder = unsafe { IntrinsicBuilder::active(machine, &mut *vmctx) }?;
        let descriptors = JsonDescriptors::resolve(&builder, layout)?;
        let left =
            builder.descriptor(u32::try_from(left).map_err(|_| RuntimeError::BadPointer)?)?;
        let right =
            builder.descriptor(u32::try_from(right).map_err(|_| RuntimeError::BadPointer)?)?;
        if descriptor_reps(&left) != [RuntimeRep::LiftedRef]
            || descriptor_reps(&right) != [RuntimeRep::LiftedRef]
        {
            return Err(RuntimeError::BadPointer);
        }
        let mut sink = JsonSink {
            builder: &mut builder,
            d: descriptors,
            failure: None,
            left,
            right,
            input: input_range,
        };
        let mut deserializer = serde_json::Deserializer::from_reader(PollingReader {
            input: input.as_bytes(),
            offset: 0,
            machine,
        });
        let parsed = JsonSeed(&mut sink)
            .deserialize(&mut deserializer)
            .and_then(|node| deserializer.end().map(|()| node));
        if machine.prepared_call_status() != CallStatus::Success {
            return Err(RuntimeError::Cancelled);
        }
        if let Some(error) = sink.failure.take() {
            return Err(error);
        }
        let result = match parsed {
            Ok(value) => {
                let right = Arc::clone(&sink.right);
                sink.con(&right, &[IntrinsicField::Node(value)])?
            }
            Err(error) => {
                let message = sink.text(&error.to_string())?;
                let left = Arc::clone(&sink.left);
                sink.con(&left, &[IntrinsicField::Node(message)])?
            }
        };
        let word = sink.builder.word(result)?;
        unsafe { output.write(word as u64) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(_error) if machine.prepared_call_status() != CallStatus::Success => {
            machine.prepared_call_status() as i32
        }
        Err(error) => super::arrays::array_error(machine, error),
    }
}

struct PollingReader<'a> {
    input: &'a [u8],
    offset: usize,
    machine: &'a MachineState,
}

struct PollingWriter<'a> {
    output: &'a mut Vec<u8>,
    machine: &'a MachineState,
}

impl Write for PollingWriter<'_> {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        if self
            .machine
            .poll_prepared(crate::prepared_control::PreparedSafepoint::Backedge)
            != CallStatus::Success
        {
            return Err(std::io::Error::other("prepared JSON encoding cancelled"));
        }
        let count = input.len().min(4096);
        self.output.extend_from_slice(&input[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for PollingReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self
            .machine
            .poll_prepared(crate::prepared_control::PreparedSafepoint::Backedge)
            != CallStatus::Success
        {
            return Err(std::io::Error::other("prepared JSON parsing cancelled"));
        }
        let count = output
            .len()
            .min(4096)
            .min(self.input.len().saturating_sub(self.offset));
        output[..count].copy_from_slice(&self.input[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

pub(super) unsafe extern "C" fn prepared_encode_json(
    vmctx: *mut VMContext,
    reference: *mut u8,
    layout: *const JsonLayoutIds,
    output: *mut u64,
) -> i32 {
    let machine = unsafe { crate::machine_state::machine_state(vmctx) };
    if machine.prepared_call_status() != CallStatus::Success {
        return machine.prepared_call_status() as i32;
    }
    let result = (|| -> Result<(), EncodeFailure> {
        if output.is_null() || layout.is_null() {
            return Err(RuntimeError::BadPointer.into());
        }
        let mut builder = unsafe { IntrinsicBuilder::active(machine, &mut *vmctx) }?;
        let descriptors = JsonDescriptors::resolve(&builder, layout)?;
        let ids = EncoderIds::resolve(&builder, layout)?;
        let input = builder.push_root(reference as usize)?;
        let mut bytes = Vec::new();
        JsonEncoder {
            builder: &mut builder,
            ids,
            ancestors: ValueAncestors::default(),
            steps: 0,
            #[cfg(test)]
            identity_comparisons: 0,
        }
        .write_value(input, RuntimeRep::LiftedRef, 0, &mut bytes)?;
        let payload = builder.bytes(&bytes)?;
        let text = builder.constructor(
            &descriptors.text,
            &[
                IntrinsicField::Node(payload),
                IntrinsicField::Bits(int_bits(0)),
                IntrinsicField::Bits(int_bits(bytes.len() as i64)),
            ],
        )?;
        unsafe { output.write(builder.word(text)? as u64) };
        Ok(())
    })();
    match result {
        Ok(()) => CallStatus::Success as i32,
        Err(EncodeFailure::Status(status)) => status as i32,
        Err(EncodeFailure::Runtime(error)) => super::arrays::array_error(machine, error),
    }
}

#[derive(Debug)]
enum EncodeFailure {
    Runtime(RuntimeError),
    Status(CallStatus),
}

impl From<RuntimeError> for EncodeFailure {
    fn from(value: RuntimeError) -> Self {
        Self::Runtime(value)
    }
}

impl From<CallStatus> for EncodeFailure {
    fn from(value: CallStatus) -> Self {
        Self::Status(value)
    }
}

#[derive(Clone, Copy)]
struct EncoderIds {
    object: DataConId,
    array: DataConId,
    string: DataConId,
    number: DataConId,
    bool_: DataConId,
    null: DataConId,
    bin: DataConId,
    tip: DataConId,
    true_: DataConId,
    false_: DataConId,
    cons: DataConId,
    nil: DataConId,
    scientific: DataConId,
    is: DataConId,
    ip: DataConId,
    in_: DataConId,
    text: DataConId,
    i_hash: DataConId,
}

impl EncoderIds {
    fn resolve(
        builder: &IntrinsicBuilder<'_>,
        layout: *const JsonLayoutIds,
    ) -> Result<Self, RuntimeError> {
        let layout = unsafe { layout.as_ref() }.ok_or(RuntimeError::BadPointer)?;
        let id = |index: u32| {
            builder
                .program
                .interned_constructors
                .get(index as usize)
                .map(|(decl, _)| decl.host_id)
                .ok_or(RuntimeError::BadPointer)
        };
        Ok(Self {
            object: id(layout.object)?,
            array: id(layout.array)?,
            string: id(layout.string)?,
            number: id(layout.number)?,
            bool_: id(layout.bool_)?,
            null: id(layout.null)?,
            bin: id(layout.map_bin)?,
            tip: id(layout.map_tip)?,
            true_: id(layout.true_)?,
            false_: id(layout.false_)?,
            cons: id(layout.cons)?,
            nil: id(layout.nil)?,
            scientific: id(layout.scientific)?,
            is: id(layout.integer_small)?,
            ip: id(layout.integer_positive)?,
            in_: id(layout.integer_negative)?,
            text: id(layout.text)?,
            i_hash: id(layout.int)?,
        })
    }
}

struct JsonEncoder<'a, 'b> {
    builder: &'a mut IntrinsicBuilder<'b>,
    ids: EncoderIds,
    ancestors: ValueAncestors,
    steps: usize,
    #[cfg(test)]
    identity_comparisons: usize,
}

enum MapAction {
    Enter((IntrinsicNode, RuntimeRep), bool),
    Emit((IntrinsicNode, RuntimeRep), (IntrinsicNode, RuntimeRep)),
    Leave(IntrinsicNode, bool),
}

/// Recursive JSON values retain only their active ancestor roots. The JSON
/// nesting limit bounds the linear identity scan, so shared acyclic values do
/// not need a process-wide or traversal-wide identity inventory.
#[derive(Default)]
struct ValueAncestors {
    nodes: Vec<IntrinsicNode>,
}

impl ValueAncestors {
    fn enter(
        &mut self,
        core: &ConstructionCore,
        node: IntrinsicNode,
        #[cfg(test)] identity_comparisons: &mut usize,
    ) -> Result<(), RuntimeError> {
        let identity = core.word(node).map_err(|_| RuntimeError::BadPointer)? & !7;
        for ancestor in self.nodes.iter().rev() {
            #[cfg(test)]
            {
                *identity_comparisons += 1;
            }
            if core.word(*ancestor).map_err(|_| RuntimeError::BadPointer)? & !7 == identity {
                return Err(RuntimeError::BlackHole);
            }
        }
        self.nodes
            .try_reserve(1)
            .map_err(|_| RuntimeError::HeapOverflow)?;
        self.nodes.push(node);
        Ok(())
    }

    fn leave(&mut self, node: IntrinsicNode) -> Result<(), RuntimeError> {
        if self.nodes.pop() == Some(node) {
            Ok(())
        } else {
            Err(RuntimeError::BadPointer)
        }
    }
}

/// Brent's cycle detector retains one collector-updated checkpoint, rather
/// than every list spine node. Inspection compares roots before ordinary
/// traversal expands the current node, so it never forces a lookahead solely
/// to check for a cycle.
struct BrentCycle {
    checkpoint: Option<IntrinsicNode>,
    power: usize,
    distance: usize,
    #[cfg(test)]
    metrics: BrentMetrics,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BrentMetrics {
    checkpoint_registrations: usize,
    checkpoint_releases: usize,
    identity_comparisons: usize,
}

impl BrentCycle {
    fn new(
        core: &mut ConstructionCore,
        machine: &MachineState,
        node: (IntrinsicNode, RuntimeRep),
    ) -> Result<Self, RuntimeError> {
        let checkpoint = core.push_word(
            machine,
            core.word(node.0).map_err(|_| RuntimeError::BadPointer)?,
            node.1,
        )?;
        Ok(Self {
            checkpoint: Some(checkpoint),
            power: 1,
            distance: 0,
            #[cfg(test)]
            metrics: BrentMetrics {
                checkpoint_registrations: 1,
                ..BrentMetrics::default()
            },
        })
    }

    fn inspect(
        &mut self,
        core: &mut ConstructionCore,
        machine: &MachineState,
        node: (IntrinsicNode, RuntimeRep),
    ) -> Result<(), RuntimeError> {
        let checkpoint = self.checkpoint.ok_or(RuntimeError::BadPointer)?;
        if self.distance != 0 {
            #[cfg(test)]
            {
                self.metrics.identity_comparisons += 1;
            }
            if core
                .word(checkpoint)
                .map_err(|_| RuntimeError::BadPointer)?
                & !7
                == core.word(node.0).map_err(|_| RuntimeError::BadPointer)? & !7
            {
                return Err(RuntimeError::BlackHole);
            }
        }
        if self.distance == self.power {
            let next_power = self
                .power
                .checked_mul(2)
                .ok_or(RuntimeError::HeapOverflow)?;
            let replacement = core.push_word(
                machine,
                core.word(node.0).map_err(|_| RuntimeError::BadPointer)?,
                node.1,
            )?;
            core.consume(machine, checkpoint)
                .map_err(|_| RuntimeError::BadPointer)?;
            self.checkpoint = Some(replacement);
            self.power = next_power;
            self.distance = 0;
            #[cfg(test)]
            {
                self.metrics.checkpoint_registrations += 1;
                self.metrics.checkpoint_releases += 1;
            }
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<(), RuntimeError> {
        self.distance = self
            .distance
            .checked_add(1)
            .ok_or(RuntimeError::HeapOverflow)?;
        Ok(())
    }

    fn finish(
        &mut self,
        core: &mut ConstructionCore,
        machine: &MachineState,
    ) -> Result<(), RuntimeError> {
        let checkpoint = self.checkpoint.take().ok_or(RuntimeError::BadPointer)?;
        core.consume(machine, checkpoint)
            .map_err(|_| RuntimeError::BadPointer)?;
        #[cfg(test)]
        {
            self.metrics.checkpoint_releases += 1;
        }
        Ok(())
    }

    #[cfg(test)]
    fn metrics(&self) -> BrentMetrics {
        self.metrics
    }
}

/// Address membership is valid only for one collection generation. Retained
/// nodes are collector-updated witnesses, so rebuilding the index preserves
/// both cycle detection and distinct acyclic objects after relocation.
#[derive(Default)]
struct MovingIdentities {
    generation: u64,
    nodes: HashMap<usize, IntrinsicNode>,
}

impl MovingIdentities {
    fn refresh(
        &mut self,
        generation: u64,
        mut word: impl FnMut(IntrinsicNode) -> Result<usize, RuntimeError>,
    ) -> Result<(), RuntimeError> {
        if self.generation != generation {
            let mut nodes = HashMap::new();
            nodes
                .try_reserve(self.nodes.len())
                .map_err(|_| RuntimeError::HeapOverflow)?;
            for node in self.nodes.values() {
                nodes.insert(word(*node)? & !7, *node);
            }
            self.nodes = nodes;
            self.generation = generation;
        }
        Ok(())
    }

    fn insert(
        &mut self,
        builder: &IntrinsicBuilder<'_>,
        node: IntrinsicNode,
    ) -> Result<bool, RuntimeError> {
        self.refresh(builder.machine.gc_generation(), |node| builder.word(node))?;
        let identity = builder.word(node)? & !7;
        if self.nodes.contains_key(&identity) {
            return Ok(false);
        }
        self.nodes
            .try_reserve(1)
            .map_err(|_| RuntimeError::HeapOverflow)?;
        self.nodes.insert(identity, node);
        Ok(true)
    }

    fn remove(
        &mut self,
        builder: &IntrinsicBuilder<'_>,
        node: IntrinsicNode,
    ) -> Result<bool, RuntimeError> {
        self.refresh(builder.machine.gc_generation(), |node| builder.word(node))?;
        Ok(self.nodes.remove(&(builder.word(node)? & !7)).is_some())
    }

    #[cfg(test)]
    fn insert_core(
        &mut self,
        core: &ConstructionCore,
        generation: u64,
        node: IntrinsicNode,
    ) -> Result<bool, RuntimeError> {
        self.refresh(generation, |node| {
            core.word(node).map_err(|_| RuntimeError::BadPointer)
        })?;
        let identity = core.word(node).map_err(|_| RuntimeError::BadPointer)? & !7;
        if self.nodes.contains_key(&identity) {
            return Ok(false);
        }
        self.nodes
            .try_reserve(1)
            .map_err(|_| RuntimeError::HeapOverflow)?;
        self.nodes.insert(identity, node);
        Ok(true)
    }

    #[cfg(test)]
    fn remove_core(
        &mut self,
        core: &ConstructionCore,
        generation: u64,
        node: IntrinsicNode,
    ) -> Result<bool, RuntimeError> {
        self.refresh(generation, |node| {
            core.word(node).map_err(|_| RuntimeError::BadPointer)
        })?;
        Ok(self
            .nodes
            .remove(&(core.word(node).map_err(|_| RuntimeError::BadPointer)? & !7))
            .is_some())
    }
}

impl JsonEncoder<'_, '_> {
    fn step(&mut self) -> Result<(), EncodeFailure> {
        self.steps = self.steps.saturating_add(1);
        if self.steps % 1024 == 0 {
            self.poll_now()?;
        }
        Ok(())
    }

    fn poll_now(&self) -> Result<(), EncodeFailure> {
        let status = self
            .builder
            .machine
            .poll_prepared(crate::prepared_control::PreparedSafepoint::Backedge);
        if status == CallStatus::Success {
            Ok(())
        } else {
            Err(status.into())
        }
    }

    fn frame(
        &mut self,
        node: IntrinsicNode,
        rep: RuntimeRep,
    ) -> Result<super::observe::ObservationFrame<(IntrinsicNode, RuntimeRep)>, EncodeFailure> {
        self.step()?;
        self.builder.expand(node, rep).map_err(Into::into)
    }

    fn constructor(
        &mut self,
        node: IntrinsicNode,
        rep: RuntimeRep,
    ) -> Result<(DataConId, Vec<(IntrinsicNode, RuntimeRep)>), EncodeFailure> {
        match self.frame(node, rep)? {
            super::observe::ObservationFrame::Constructor(id, fields) => Ok((id, fields)),
            _ => Err(RuntimeError::BadPointer.into()),
        }
    }

    fn write_value(
        &mut self,
        node: IntrinsicNode,
        rep: RuntimeRep,
        depth: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), EncodeFailure> {
        if depth > MAX_JSON_VALUE_DEPTH {
            return Err(RuntimeError::StackOverflow.into());
        }
        self.ancestors.enter(
            &self.builder.core,
            node,
            #[cfg(test)]
            &mut self.identity_comparisons,
        )?;
        let result = (|| {
            let (id, fields) = self.constructor(node, rep)?;
            let result = (|| {
                if id == self.ids.null && fields.is_empty() {
                    out.extend_from_slice(b"null");
                } else if id == self.ids.string && fields.len() == 1 {
                    self.write_text(fields[0], out)?;
                } else if id == self.ids.bool_ && fields.len() == 1 {
                    self.write_bool(fields[0], out)?;
                } else if id == self.ids.number && fields.len() == 1 {
                    self.write_number(fields[0], out)?;
                } else if id == self.ids.array && fields.len() == 1 {
                    self.write_list(fields[0], depth + 1, out)?;
                } else if id == self.ids.object && fields.len() == 1 {
                    self.write_map(fields[0], depth + 1, out)?;
                } else {
                    return Err(RuntimeError::BadPointer.into());
                }
                Ok(())
            })();
            self.finish_nodes(result, fields.into_iter().map(|field| field.0))
        })();
        let leave = self.ancestors.leave(node).map_err(EncodeFailure::from);
        result.and(leave)
    }

    fn write_bool(
        &mut self,
        field: (IntrinsicNode, RuntimeRep),
        out: &mut Vec<u8>,
    ) -> Result<(), EncodeFailure> {
        let (id, fields) = self.constructor(field.0, field.1)?;
        let result = if !fields.is_empty() {
            Err(RuntimeError::BadPointer.into())
        } else if id == self.ids.true_ {
            out.extend_from_slice(b"true");
            Ok(())
        } else if id == self.ids.false_ {
            out.extend_from_slice(b"false");
            Ok(())
        } else {
            Err(RuntimeError::BadPointer.into())
        };
        self.finish_nodes(result, fields.into_iter().map(|field| field.0))
    }

    fn write_list(
        &mut self,
        field: (IntrinsicNode, RuntimeRep),
        depth: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), EncodeFailure> {
        out.push(b'[');
        let mut cycle = BrentCycle::new(&mut self.builder.core, self.builder.machine, field)?;
        let result = self.write_list_body(field, depth, out, &mut cycle);
        let cleanup = cycle
            .finish(&mut self.builder.core, self.builder.machine)
            .map_err(EncodeFailure::from);
        #[cfg(test)]
        {
            self.identity_comparisons += cycle.metrics().identity_comparisons;
        }
        match result {
            Err(error) => Err(error),
            Ok(value) => cleanup.map(|()| value),
        }
    }

    fn write_list_body(
        &mut self,
        mut field: (IntrinsicNode, RuntimeRep),
        depth: usize,
        out: &mut Vec<u8>,
        cycle: &mut BrentCycle,
    ) -> Result<(), EncodeFailure> {
        let mut first = true;
        let mut owned_current = false;
        loop {
            if let Err(error) = cycle.inspect(&mut self.builder.core, self.builder.machine, field) {
                if owned_current {
                    let _ = self.builder.release_node(field.0);
                }
                return Err(error.into());
            }
            let expanded = self.constructor(field.0, field.1);
            if owned_current {
                let release = self
                    .builder
                    .release_node(field.0)
                    .map_err(EncodeFailure::from);
                if let Err(error) = expanded {
                    return Err(error);
                }
                release?;
            }
            let (id, fields) = expanded?;
            if id == self.ids.nil && fields.is_empty() {
                out.push(b']');
                return Ok(());
            }
            if id != self.ids.cons || fields.len() != 2 {
                return self.finish_nodes(
                    Err(RuntimeError::BadPointer.into()),
                    fields.into_iter().map(|field| field.0),
                );
            }
            let head = fields[0];
            let tail = fields[1];
            if !first {
                out.push(b',');
            }
            first = false;
            let value = self.write_value(head.0, head.1, depth, out);
            let value = self.finish_nodes(value, std::iter::once(head.0));
            if let Err(error) = value {
                let _ = self.builder.release_node(tail.0);
                return Err(error);
            }
            field = tail;
            owned_current = true;
            cycle.advance()?;
        }
    }

    fn write_map(
        &mut self,
        root: (IntrinsicNode, RuntimeRep),
        depth: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), EncodeFailure> {
        out.push(b'{');
        let mut first = true;
        let mut active = MovingIdentities::default();
        let mut actions = vec![MapAction::Enter(root, false)];
        let result = (|| {
            while let Some(action) = actions.pop() {
                match action {
                    MapAction::Enter(node, owned) => {
                        let expanded = self.constructor(node.0, node.1);
                        let (id, mut fields) = match expanded {
                            Ok(expanded) => expanded,
                            Err(error) => {
                                if owned {
                                    let _ = self.builder.release_node(node.0);
                                }
                                return Err(error);
                            }
                        };
                        if id == self.ids.tip && fields.is_empty() {
                            if owned {
                                self.builder.release_node(node.0)?;
                            }
                            continue;
                        }
                        if id != self.ids.bin || fields.len() != 5 {
                            let result = self.finish_nodes(
                                Err(RuntimeError::BadPointer.into()),
                                fields.into_iter().map(|field| field.0),
                            );
                            if owned {
                                let _ = self.builder.release_node(node.0);
                            }
                            return result;
                        }
                        if !active.insert(self.builder, node.0)? {
                            let result = self.finish_nodes(
                                Err(RuntimeError::BlackHole.into()),
                                fields.into_iter().map(|field| field.0),
                            );
                            if owned {
                                let _ = self.builder.release_node(node.0);
                            }
                            return result;
                        }
                        let right = fields.pop().expect("validated map fields");
                        let left = fields.pop().expect("validated map fields");
                        let value = fields.pop().expect("validated map fields");
                        let key = fields.pop().expect("validated map fields");
                        let size = fields.pop().expect("validated map fields");
                        self.builder.release_node(size.0)?;
                        actions.push(MapAction::Leave(node.0, owned));
                        actions.push(MapAction::Enter(right, true));
                        actions.push(MapAction::Emit(key, value));
                        actions.push(MapAction::Enter(left, true));
                    }
                    MapAction::Emit(key, value) => {
                        if !first {
                            out.push(b',');
                        }
                        first = false;
                        let result = (|| {
                            self.write_text(key, out)?;
                            out.push(b':');
                            self.write_value(value.0, value.1, depth, out)
                        })();
                        self.finish_nodes(result, [key.0, value.0])?;
                    }
                    MapAction::Leave(node, owned) => {
                        if !active.remove(self.builder, node)? {
                            return Err(RuntimeError::BadPointer.into());
                        }
                        if owned {
                            self.builder.release_node(node)?;
                        }
                    }
                }
            }
            out.push(b'}');
            Ok(())
        })();
        if result.is_err() {
            for action in actions {
                match action {
                    MapAction::Enter((node, _), true) | MapAction::Leave(node, true) => {
                        let _ = self.builder.release_node(node);
                    }
                    MapAction::Emit(key, value) => {
                        let _ = self.builder.release_node(key.0);
                        let _ = self.builder.release_node(value.0);
                    }
                    _ => {}
                }
            }
        }
        result
    }

    fn write_text(
        &mut self,
        field: (IntrinsicNode, RuntimeRep),
        out: &mut Vec<u8>,
    ) -> Result<(), EncodeFailure> {
        let (id, fields) = self.constructor(field.0, field.1)?;
        let result = (|| {
            if id != self.ids.text || fields.len() != 3 {
                return Err(RuntimeError::BadPointer.into());
            }
            let bytes = self.bytes(fields[0])?;
            let offset = self.int(fields[1])?;
            let length = self.int(fields[2])?;
            let offset = usize::try_from(offset).map_err(|_| RuntimeError::BadPointer)?;
            let length = usize::try_from(length).map_err(|_| RuntimeError::BadPointer)?;
            let end = offset.checked_add(length).ok_or(RuntimeError::BadPointer)?;
            let text = std::str::from_utf8(bytes.get(offset..end).ok_or(RuntimeError::BadPointer)?)
                .map_err(|_| RuntimeError::BadPointer)?;
            serde_json::to_writer(
                PollingWriter {
                    output: out,
                    machine: self.builder.machine,
                },
                text,
            )
            .map_err(|_| RuntimeError::BadPointer)?;
            Ok(())
        })();
        self.finish_nodes(result, fields.into_iter().map(|field| field.0))
    }

    fn write_number(
        &mut self,
        field: (IntrinsicNode, RuntimeRep),
        out: &mut Vec<u8>,
    ) -> Result<(), EncodeFailure> {
        let (id, fields) = self.constructor(field.0, field.1)?;
        let result = (|| {
            if id != self.ids.scientific || fields.len() != 2 {
                return Err(RuntimeError::BadPointer.into());
            }
            let coefficient = self.integer(fields[0])?;
            let exponent = self.int(fields[1])?;
            let decimal = tidepool_bridge::decimal::Decimal::from_parts(&coefficient, exponent)
                .map_err(|_| RuntimeError::BadPointer)?;
            out.extend_from_slice(decimal.render().as_bytes());
            Ok(())
        })();
        self.finish_nodes(result, fields.into_iter().map(|field| field.0))
    }

    fn integer(&mut self, field: (IntrinsicNode, RuntimeRep)) -> Result<String, EncodeFailure> {
        let (id, fields) = self.constructor(field.0, field.1)?;
        let result = (|| {
            if id == self.ids.is && fields.len() == 1 {
                return Ok(self.int(fields[0])?.to_string());
            }
            if (id == self.ids.ip || id == self.ids.in_) && fields.len() == 1 {
                let magnitude = self.bytes(fields[0])?;
                let mut value = self.bignat_decimal(&magnitude)?;
                if id == self.ids.in_ {
                    value.insert(0, '-');
                }
                return Ok(value);
            }
            Err(RuntimeError::BadPointer.into())
        })();
        self.finish_nodes(result, fields.into_iter().map(|field| field.0))
    }

    fn int(&mut self, field: (IntrinsicNode, RuntimeRep)) -> Result<i64, EncodeFailure> {
        match self.frame(field.0, field.1)? {
            super::observe::ObservationFrame::Leaf(tidepool_bridge::HaskellValue::Lit(
                tidepool_repr::Literal::LitInt(v),
            )) => Ok(v),
            super::observe::ObservationFrame::Constructor(id, fields) => {
                let result = if id == self.ids.i_hash && fields.len() == 1 {
                    self.int(fields[0])
                } else {
                    Err(RuntimeError::BadPointer.into())
                };
                self.finish_nodes(result, fields.into_iter().map(|field| field.0))
            }
            _ => Err(RuntimeError::BadPointer.into()),
        }
    }

    fn finish_nodes<T>(
        &mut self,
        result: Result<T, EncodeFailure>,
        nodes: impl IntoIterator<Item = IntrinsicNode>,
    ) -> Result<T, EncodeFailure> {
        let mut cleanup = Ok(());
        for node in nodes {
            if let Err(error) = self.builder.release_node(node) {
                if cleanup.is_ok() {
                    cleanup = Err(error.into());
                }
            }
        }
        match result {
            Err(error) => Err(error),
            Ok(value) => cleanup.map(|()| value),
        }
    }

    fn bytes(&mut self, field: (IntrinsicNode, RuntimeRep)) -> Result<Vec<u8>, EncodeFailure> {
        match self.frame(field.0, field.1)? {
            super::observe::ObservationFrame::Leaf(tidepool_bridge::HaskellValue::ByteArray(
                ref bytes,
            )) => {
                let bytes = bytes.lock().map_err(|_| RuntimeError::BadPointer)?;
                let mut copy = Vec::new();
                copy.try_reserve_exact(bytes.len())
                    .map_err(|_| RuntimeError::HeapOverflow)?;
                for chunk in bytes.chunks(4096) {
                    self.poll_now()?;
                    copy.extend_from_slice(chunk);
                }
                Ok(copy)
            }
            super::observe::ObservationFrame::Leaf(tidepool_bridge::HaskellValue::Lit(
                tidepool_repr::Literal::LitByteArray(ref bytes),
            )) => {
                let mut copy = Vec::new();
                copy.try_reserve_exact(bytes.len())
                    .map_err(|_| RuntimeError::HeapOverflow)?;
                for chunk in bytes.chunks(4096) {
                    self.poll_now()?;
                    copy.extend_from_slice(chunk);
                }
                Ok(copy)
            }
            _ => Err(RuntimeError::BadPointer.into()),
        }
    }

    fn bignat_decimal(&mut self, bytes: &[u8]) -> Result<String, EncodeFailure> {
        let mut limbs = bytes
            .chunks(8)
            .map(|chunk| {
                let mut word = [0_u8; 8];
                word[..chunk.len()].copy_from_slice(chunk);
                u64::from_le_bytes(word)
            })
            .collect::<Vec<_>>();
        while limbs.last() == Some(&0) {
            limbs.pop();
        }
        if limbs.is_empty() {
            return Ok("0".into());
        }
        let divisor = 10_000_000_000_000_000_000_u128;
        let mut groups = Vec::new();
        while !limbs.is_empty() {
            let mut remainder = 0_u128;
            for (index, limb) in limbs.iter_mut().enumerate().rev() {
                if index % 1024 == 0 {
                    self.poll_now()?;
                }
                let value = (remainder << 64) | u128::from(*limb);
                *limb = (value / divisor) as u64;
                remainder = value % divisor;
            }
            groups.push(remainder as u64);
            while limbs.last() == Some(&0) {
                limbs.pop();
            }
        }
        let mut result = groups.pop().unwrap_or(0).to_string();
        for group in groups.into_iter().rev() {
            use std::fmt::Write as _;
            write!(&mut result, "{group:019}").map_err(|_| RuntimeError::HeapOverflow)?;
        }
        Ok(result)
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
        let Some(first) = access.next_key_seed(JsonKeySeed(self.0.input.clone()))? else {
            let map = self.0.map(BTreeMap::new()).map_err(A::Error::custom)?;
            let d = Arc::clone(&self.0.d.object);
            return self
                .0
                .con(&d, &[IntrinsicField::Node(map)])
                .map_err(A::Error::custom);
        };
        if matches!(first, JsonKey::NumberToken) {
            let token = access.next_value::<String>()?;
            return self.0.number(&token).map_err(A::Error::custom);
        }
        let JsonKey::Object(first) = first else {
            unreachable!()
        };
        let mut entries = BTreeMap::new();
        let value = access.next_value_seed(JsonSeed(self.0))?;
        if let Some(displaced) = entries.insert(first, value) {
            self.0.release_node(displaced).map_err(A::Error::custom)?;
        }
        while let Some(key) = access.next_key_seed(JsonKeySeed(self.0.input.clone()))? {
            let JsonKey::Object(key) = key else {
                return Err(A::Error::custom(
                    "number token is only valid as a numeric wrapper",
                ));
            };
            let value = access.next_value_seed(JsonSeed(self.0))?;
            if let Some(displaced) = entries.insert(key, value) {
                self.0.release_node(displaced).map_err(A::Error::custom)?;
            }
        }
        let map = self.0.map(entries).map_err(A::Error::custom)?;
        let d = Arc::clone(&self.0.d.object);
        self.0
            .con(&d, &[IntrinsicField::Node(map)])
            .map_err(A::Error::custom)
    }
}

enum JsonKey {
    NumberToken,
    Object(String),
}

struct JsonKeySeed(std::ops::Range<usize>);

impl<'de> DeserializeSeed<'de> for JsonKeySeed {
    type Value = JsonKey;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_string(JsonKeyVisitor(self.0))
    }
}

struct JsonKeyVisitor(std::ops::Range<usize>);

impl Visitor<'_> for JsonKeyVisitor {
    type Value = JsonKey;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object key")
    }

    fn visit_borrowed_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let pointer = value.as_ptr() as usize;
        let from_input = pointer
            .checked_add(value.len())
            .is_some_and(|end| pointer >= self.0.start && end <= self.0.end);
        if value == NUMBER_TOKEN && !from_input {
            Ok(JsonKey::NumberToken)
        } else {
            Ok(JsonKey::Object(value.to_owned()))
        }
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(JsonKey::Object(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(JsonKey::Object(value))
    }
}

struct IntrinsicBuilder<'a> {
    machine: &'a MachineState,
    vmctx: &'a mut VMContext,
    core: ConstructionCore,
    program: &'a CompiledProgram,
    starts: Vec<u64>,
    scanned_words: usize,
    indexed_generation: u64,
}

impl Drop for IntrinsicBuilder<'_> {
    fn drop(&mut self) {
        self.core.release(self.machine);
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
            core: ConstructionCore::new(program as *const _ as u64),
            program,
            starts: Vec::new(),
            scanned_words: 0,
            indexed_generation: u64::MAX,
        })
    }

    fn descriptor(&self, index: u32) -> Result<Arc<ObjectDescriptor>, RuntimeError> {
        self.program
            .interned_constructors
            .get(index as usize)
            .map(|(_, descriptor)| Arc::clone(descriptor))
            .ok_or(RuntimeError::BadPointer)
    }

    fn collect(
        machine: &MachineState,
        vmctx: &mut VMContext,
        needed: usize,
    ) -> Result<(), RuntimeError> {
        let raw = unsafe { prepared_gc_trigger(vmctx, needed) };
        let status = CallStatus::from_raw(i64::from(raw)).map_err(|_| RuntimeError::BadPointer)?;
        if status == CallStatus::Success && machine.prepared_call_status() == CallStatus::Success {
            Ok(())
        } else {
            Err(RuntimeError::BadPointer)
        }
    }

    fn push_word(&mut self, word: usize, rep: RuntimeRep) -> Result<IntrinsicNode, RuntimeError> {
        self.core.push_word(self.machine, word, rep)
    }

    fn push_root(&mut self, word: usize) -> Result<IntrinsicNode, RuntimeError> {
        self.push_word(word, RuntimeRep::LiftedRef)
    }

    /// Consume one traversal-owned temporary root after its final reader has
    /// finished. The free-list capacity was admitted before the slot was
    /// published, so this cleanup cannot allocate while reporting an error.
    fn release_node(&mut self, node: IntrinsicNode) -> Result<(), RuntimeError> {
        self.core
            .consume(self.machine, node)
            .map_err(|_| RuntimeError::BadPointer)
    }

    fn force(&mut self, node: IntrinsicNode) -> Result<(), CallStatus> {
        if self.machine.prepared_call_status() != CallStatus::Success {
            return Err(self.machine.prepared_call_status());
        }
        let reserve = self
            .program
            .pipeline
            .native_frame_maximum()
            .checked_mul(2)
            .ok_or(CallStatus::IntegrityFailure)?;
        let bounds = super::safepoint::NativeStackBounds::current().map_err(|cause| {
            self.machine.set_first_cause(cause);
            self.machine.prepared_call_status()
        })?;
        bounds
            .ensure_current_frame_reserve(reserve)
            .map_err(|cause| {
                self.machine.set_first_cause(cause);
                self.machine.prepared_call_status()
            })?;
        let input = self.word(node).map_err(|cause| {
            self.machine.set_first_cause(cause);
            self.machine.prepared_call_status()
        })?;
        let output = self
            .core
            .slot(node)
            .map_err(|_| CallStatus::IntegrityFailure)?;
        let pointer = self
            .program
            .pipeline
            .get_function_ptr(self.program.prepared_force_adapter());
        let adapter: unsafe extern "C" fn(*mut VMContext, *mut u64, usize) -> i32 =
            unsafe { std::mem::transmute(pointer) };
        let raw = unsafe { adapter(self.vmctx, output, input) };
        let status =
            CallStatus::from_raw(i64::from(raw)).map_err(|_| CallStatus::IntegrityFailure)?;
        if status != CallStatus::Success
            || self.machine.prepared_call_status() != CallStatus::Success
        {
            return Err(status);
        }
        Ok(())
    }

    fn expand(
        &mut self,
        node: IntrinsicNode,
        rep: RuntimeRep,
    ) -> Result<super::observe::ObservationFrame<(IntrinsicNode, RuntimeRep)>, CallStatus> {
        if rep == RuntimeRep::LiftedRef {
            self.force(node)?;
        }
        let word = self.word(node).map_err(|cause| {
            self.machine.set_first_cause(cause);
            self.machine.prepared_call_status()
        })?;
        let old_space =
            unsafe { self.machine.prepared_old_space() }.ok_or(CallStatus::IntegrityFailure)?;
        let (statics, registry) = unsafe { self.machine.active_intrinsic_observation() }
            .ok_or(CallStatus::IntegrityFailure)?;
        let mut budget = super::observe::ObservationBudget {
            remaining: usize::MAX,
            limit: usize::MAX,
        };
        let frame = {
            let heap = super::forcing::current_heap(
                self.machine,
                self.vmctx,
                statics,
                registry,
                old_space,
                &mut self.starts,
                &mut self.scanned_words,
                &mut self.indexed_generation,
            )
            .map_err(|_| CallStatus::IntegrityFailure)?;
            heap.expand(
                super::observe::ObservationSeed { word, rep },
                &mut budget,
                crate::observation::BudgetPolicy::Complete,
            )
            .map_err(|_| CallStatus::IntegrityFailure)?
        };
        match frame {
            super::observe::ObservationFrame::Leaf(value) => {
                Ok(super::observe::ObservationFrame::Leaf(value))
            }
            super::observe::ObservationFrame::Constructor(identity, mut fields) => {
                // Observation expansion returns children in worklist order.
                // Direct consumers need the constructor's logical field order.
                fields.reverse();
                let mut rooted = Vec::new();
                rooted.try_reserve_exact(fields.len()).map_err(|_| {
                    self.machine.set_first_cause(RuntimeError::HeapOverflow);
                    self.machine.prepared_call_status()
                })?;
                for field in fields {
                    let node = self.push_word(field.word, field.rep).map_err(|cause| {
                        self.machine.set_first_cause(cause);
                        self.machine.prepared_call_status()
                    })?;
                    rooted.push((node, field.rep));
                }
                Ok(super::observe::ObservationFrame::Constructor(
                    identity, rooted,
                ))
            }
        }
    }

    fn word(&self, node: IntrinsicNode) -> Result<usize, RuntimeError> {
        self.core.word(node).map_err(|_| RuntimeError::BadPointer)
    }

    fn constructor(
        &mut self,
        descriptor: &ObjectDescriptor,
        fields: &[IntrinsicField],
    ) -> Result<IntrinsicNode, RuntimeError> {
        // JSON decode builds a tree. Each completed child is retained through
        // the parent's capacity collection, then consumed after publication;
        // no completed subtree remains as an unrelated temporary GC root.
        let mut consumed = Vec::new();
        consumed
            .try_reserve_exact(fields.len())
            .map_err(|_| RuntimeError::HeapOverflow)?;
        for field in fields {
            if let IntrinsicField::Node(node) = field {
                if consumed.contains(node) || self.core.word(*node).is_err() {
                    return Err(RuntimeError::BadPointer);
                }
                consumed.push(*node);
            }
        }
        self.core
            .constructor(
                self.machine,
                self.vmctx,
                descriptor,
                fields.len(),
                &consumed,
                Self::collect,
                |core, values| {
                    for (output, field) in values.iter_mut().zip(fields) {
                        *output = match field {
                            IntrinsicField::Node(node) => DescriptorValue::Managed(
                                core.word(*node).map_err(|_| RuntimeError::BadPointer)? as *mut u8,
                            ),
                            IntrinsicField::Bits(bits) => DescriptorValue::Bits(*bits),
                        };
                    }
                    Ok(())
                },
            )
            .map_err(|error| match error {
                ConstructionError::Runtime(error) | ConstructionError::Operation(error) => error,
                ConstructionError::TooLarge(_) => RuntimeError::HeapOverflow,
                ConstructionError::Marshal(_) => RuntimeError::BadPointer,
                ConstructionError::Storage(_) => RuntimeError::HeapOverflow,
            })
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<IntrinsicNode, RuntimeError> {
        let descriptor = Arc::clone(&self.program.externals.bytes_array);
        self.core
            .bytes(self.machine, self.vmctx, &descriptor, bytes, Self::collect)
            .map_err(|error| match error {
                ConstructionError::Runtime(error) | ConstructionError::Operation(error) => error,
                ConstructionError::TooLarge(_) => RuntimeError::HeapOverflow,
                ConstructionError::Marshal(_) => RuntimeError::BadPointer,
                ConstructionError::Storage(_) => RuntimeError::HeapOverflow,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_state::MachineDisposition;
    use crate::prepared_program::safepoint::NativeStackBounds;
    use crate::prepared_program::{ActiveIntrinsicScope, DescriptorMeaning};
    use tidepool_repr::execution_schema::{
        link_program, testing, CheckedLayout, ConstructorDecl, FieldLayout, MachineImports,
        StorageLayout,
    };

    fn rooted_words(
        roots: &mut ConstructionCore,
        machine: &MachineState,
        words: impl IntoIterator<Item = usize>,
    ) -> Vec<IntrinsicNode> {
        words
            .into_iter()
            .map(|word| {
                roots
                    .push_word(machine, word | 1, RuntimeRep::LiftedRef)
                    .unwrap()
            })
            .collect()
    }

    fn constructor(identity: &str, host_id: u64, fields: Vec<RuntimeRep>) -> ConstructorDecl {
        let layout = StorageLayout::for_reps(&testing::target(), &fields).unwrap();
        ConstructorDecl {
            identity: testing::identity("JsonTraversal", identity),
            family: testing::identity("JsonTraversal", &format!("{identity}Family")),
            host_id: DataConId(host_id),
            result_rep: RuntimeRep::LiftedRef,
            strict_fields: vec![false; fields.len()],
            field_reps: fields,
            layout: CheckedLayout {
                fields: layout
                    .fields()
                    .iter()
                    .map(|field| FieldLayout {
                        rep: field.rep(),
                        offset: field.offset(),
                    })
                    .collect(),
                alignment: layout.alignment(),
                payload_size: layout.payload_size(),
                root_mask: layout
                    .fields()
                    .iter()
                    .map(|field| {
                        matches!(field.rep(), RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef)
                    })
                    .collect(),
            },
            tag: 1,
            family_size: 1,
        }
    }

    fn traversal_program() -> CompiledProgram {
        let mut wire = testing::wire_program();
        wire.constructors.extend([
            constructor("Array", 10_000, vec![RuntimeRep::LiftedRef]),
            constructor("Null", 10_001, vec![]),
            constructor(
                "Cons",
                10_002,
                vec![RuntimeRep::LiftedRef, RuntimeRep::LiftedRef],
            ),
            constructor("Nil", 10_003, vec![]),
        ]);
        let linked =
            link_program(testing::prepare(wire).unwrap(), &MachineImports::default()).unwrap();
        CompiledProgram::compile(&linked).unwrap()
    }

    fn descriptor(program: &CompiledProgram, id: DataConId) -> Arc<ObjectDescriptor> {
        program
            .descriptor_registry
            .values()
            .find_map(|metadata| match &metadata.meaning {
                DescriptorMeaning::Constructor(observation) if observation.identity == id => {
                    Some(Arc::clone(&metadata.descriptor))
                }
                _ => None,
            })
            .unwrap()
    }

    fn encode_null_array(
        width: usize,
        cancel_at: Option<usize>,
        corrupt_head: bool,
    ) -> (
        Result<String, CallStatus>,
        super::super::construction::RootOperationMetrics,
        usize,
        MachineDisposition,
    ) {
        let program = traversal_program();
        let machine = MachineState::new();
        machine.register_prepared_entries(
            std::iter::empty(),
            program
                .thunk_entries
                .iter()
                .map(|&(header, function)| (header, program.pipeline.get_function_ptr(function))),
        );
        machine.set_stack_map_registry(&program.pipeline.stack_maps);
        let statics = Arc::new(program.statics.instantiate().unwrap());
        let mut static_catalog = tidepool_heap::static_region::StaticRegionCatalog::new();
        static_catalog.insert(Arc::clone(&statics)).unwrap();
        machine
            .install_prepared_buffer_with_static_region(
                vec![0_u64; 1 << 18],
                program.descriptors.clone(),
                Some(Arc::clone(&statics)),
            )
            .unwrap();
        let (start, size) = machine.gc_active_range().unwrap();
        let mut vmctx = unsafe { VMContext::new(start, start.add(size)) };
        vmctx.machine_state = (&machine as *const MachineState).cast_mut();
        vmctx.prepared_stack_limit = NativeStackBounds::current()
            .unwrap()
            .limit_with_frame_reserve(program.pipeline.native_frame_maximum())
            .unwrap();
        let old_space = crate::old_space::OldSpace::new();
        unsafe { machine.install_prepared_old_space(&old_space) };
        let _active = ActiveIntrinsicScope::new(
            &machine,
            &program,
            &static_catalog,
            &program.descriptor_registry,
        )
        .unwrap();
        let mut builder = unsafe { IntrinsicBuilder::active(&machine, &mut vmctx) }.unwrap();
        let array = descriptor(&program, DataConId(10_000));
        let null = descriptor(&program, DataConId(10_001));
        let cons = descriptor(&program, DataConId(10_002));
        let nil = descriptor(&program, DataConId(10_003));
        let mut tail = builder.constructor(&nil, &[]).unwrap();
        for _ in 0..width {
            let value = builder.constructor(&null, &[]).unwrap();
            tail = builder
                .constructor(
                    &cons,
                    &[IntrinsicField::Node(value), IntrinsicField::Node(tail)],
                )
                .unwrap();
        }
        let list_word = builder.word(tail).unwrap();
        let value = builder
            .constructor(&array, &[IntrinsicField::Node(tail)])
            .unwrap();
        if corrupt_head {
            let list = list_word & !7;
            let stored = cons.payload().logical_to_stored()[0].unwrap() as usize;
            let offset = cons.payload().fields()[stored].offset() as usize;
            unsafe {
                (list as *mut u8)
                    .add(cons.payload_base() as usize + offset)
                    .cast::<usize>()
                    .write_unaligned(usize::MAX);
            }
        }
        if let Some(count) = cancel_at {
            machine.fail_prepared_at(
                crate::prepared_control::PreparedSafepoint::Backedge,
                count,
                RuntimeError::Cancelled,
            );
        }
        builder.core.reset_root_operation_metrics();
        let mut output = Vec::new();
        let mut encoder = JsonEncoder {
            builder: &mut builder,
            ids: EncoderIds {
                object: DataConId(u64::MAX),
                array: DataConId(10_000),
                string: DataConId(u64::MAX),
                number: DataConId(u64::MAX),
                bool_: DataConId(u64::MAX),
                null: DataConId(10_001),
                bin: DataConId(u64::MAX),
                tip: DataConId(u64::MAX),
                true_: DataConId(u64::MAX),
                false_: DataConId(u64::MAX),
                cons: DataConId(10_002),
                nil: DataConId(10_003),
                scientific: DataConId(u64::MAX),
                is: DataConId(u64::MAX),
                ip: DataConId(u64::MAX),
                in_: DataConId(u64::MAX),
                text: DataConId(u64::MAX),
                i_hash: DataConId(u64::MAX),
            },
            ancestors: ValueAncestors::default(),
            steps: 0,
            identity_comparisons: 0,
        };
        let result = encoder.write_value(value, RuntimeRep::LiftedRef, 0, &mut output);
        let comparisons = encoder.identity_comparisons;
        let metrics = encoder.builder.core.root_operation_metrics();
        drop(encoder);
        builder.release_node(value).unwrap();
        assert_eq!(machine.rust_roots_len(), 0);
        if corrupt_head {
            let Err(EncodeFailure::Status(status)) = &result else {
                panic!("corrupt managed child must return an integrity status")
            };
            let _ = super::super::run::runtime_error_for_status(&machine, *status);
        }
        let disposition = machine.disposition();
        let output = match result {
            Ok(()) => Ok(String::from_utf8(output).unwrap()),
            Err(EncodeFailure::Status(status)) => Err(status),
            Err(EncodeFailure::Runtime(error)) => panic!("unexpected encoder error: {error:?}"),
        };
        (output, metrics, comparisons, disposition)
    }

    #[test]
    fn brent_cycle_detection_covers_prefixes_and_cycle_lengths() {
        for prefix in [0, 1, 3, 8] {
            for cycle_length in [1, 2, 3, 7, 16] {
                let machine = MachineState::new();
                let mut roots = ConstructionCore::new(1);
                let unique = prefix + cycle_length;
                let words = (0..unique).map(|index| 0x1000 + index * 16);
                let mut nodes = rooted_words(&mut roots, &machine, words);
                let mut detector =
                    BrentCycle::new(&mut roots, &machine, (nodes[0], RuntimeRep::LiftedRef))
                        .unwrap();
                let mut detected = false;
                for step in 0..(4 * unique + 2) {
                    let index = if step < unique {
                        step
                    } else {
                        prefix + (step - prefix) % cycle_length
                    };
                    let word = roots.word(nodes[index]).unwrap();
                    let current = roots
                        .push_word(&machine, word, RuntimeRep::LiftedRef)
                        .unwrap();
                    match detector.inspect(&mut roots, &machine, (current, RuntimeRep::LiftedRef)) {
                        Err(RuntimeError::BlackHole) => {
                            detected = true;
                            roots.consume(&machine, current).unwrap();
                            break;
                        }
                        Ok(()) => detector.advance().unwrap(),
                        Err(error) => panic!("unexpected detector error: {error:?}"),
                    }
                    roots.consume(&machine, current).unwrap();
                }
                assert!(detected, "prefix={prefix}, cycle_length={cycle_length}");
                detector.finish(&mut roots, &machine).unwrap();
                for node in nodes.drain(..) {
                    roots.consume(&machine, node).unwrap();
                }
                assert_eq!(machine.rust_roots_len(), 0);
            }
        }
    }

    #[test]
    fn value_ancestors_reject_cycles_but_allow_shared_acyclic_values() {
        let machine = MachineState::new();
        let mut roots = ConstructionCore::new(1);
        let nodes = rooted_words(&mut roots, &machine, [0x1000, 0x2000, 0x1000]);
        let mut ancestors = ValueAncestors::default();
        let mut comparisons = 0;
        ancestors.enter(&roots, nodes[0], &mut comparisons).unwrap();
        ancestors.enter(&roots, nodes[1], &mut comparisons).unwrap();
        assert!(matches!(
            ancestors.enter(&roots, nodes[2], &mut comparisons),
            Err(RuntimeError::BlackHole)
        ));
        ancestors.leave(nodes[1]).unwrap();
        ancestors.leave(nodes[0]).unwrap();
        ancestors.enter(&roots, nodes[2], &mut comparisons).unwrap();
        ancestors.leave(nodes[2]).unwrap();
        for node in nodes {
            roots.consume(&machine, node).unwrap();
        }
        assert_eq!(machine.rust_roots_len(), 0);
    }

    #[test]
    fn brent_checkpoint_uses_collector_updated_root_identity() {
        let machine = MachineState::new();
        let mut roots = ConstructionCore::new(1);
        let nodes = rooted_words(&mut roots, &machine, [0x1000, 0x2000]);
        let mut detector =
            BrentCycle::new(&mut roots, &machine, (nodes[0], RuntimeRep::LiftedRef)).unwrap();
        detector.advance().unwrap();
        let checkpoint = detector.checkpoint.unwrap();
        unsafe {
            roots.slot(checkpoint).unwrap().write(0x9001);
            roots.slot(nodes[1]).unwrap().write(0x9001);
        }
        assert!(matches!(
            detector.inspect(&mut roots, &machine, (nodes[1], RuntimeRep::LiftedRef)),
            Err(RuntimeError::BlackHole)
        ));
        detector.finish(&mut roots, &machine).unwrap();
        for node in nodes {
            roots.consume(&machine, node).unwrap();
        }
        assert_eq!(machine.rust_roots_len(), 0);
    }

    #[test]
    fn active_map_identity_can_be_reused_after_subtree_exit() {
        let machine = MachineState::new();
        let mut roots = ConstructionCore::new(1);
        let nodes = rooted_words(&mut roots, &machine, [0x1000, 0x1000]);
        let mut active = MovingIdentities::default();
        assert!(active.insert_core(&roots, 0, nodes[0]).unwrap());
        assert!(!active.insert_core(&roots, 0, nodes[1]).unwrap());
        assert!(active.remove_core(&roots, 0, nodes[0]).unwrap());
        assert!(active.insert_core(&roots, 0, nodes[1]).unwrap());
        assert!(active.remove_core(&roots, 0, nodes[1]).unwrap());
        for node in nodes {
            roots.consume(&machine, node).unwrap();
        }
        assert_eq!(machine.rust_roots_len(), 0);
    }

    #[test]
    fn brent_identity_and_root_work_scale_linearly_and_logarithmically() {
        for width in [16_usize, 256, 4096] {
            let machine = MachineState::new();
            let mut roots = ConstructionCore::new(1);
            let nodes = rooted_words(
                &mut roots,
                &machine,
                (0..width).map(|index| 0x1000 + index * 16),
            );
            let before = roots.root_operation_metrics();
            let mut detector =
                BrentCycle::new(&mut roots, &machine, (nodes[0], RuntimeRep::LiftedRef)).unwrap();
            for node in &nodes {
                detector
                    .inspect(&mut roots, &machine, (*node, RuntimeRep::LiftedRef))
                    .unwrap();
                detector.advance().unwrap();
            }
            detector.finish(&mut roots, &machine).unwrap();
            let metrics = detector.metrics();
            let after = roots.root_operation_metrics();
            let checkpoint_roots = 1 + width.ilog2() as usize;
            assert_eq!(metrics.identity_comparisons, width - 1);
            assert_eq!(metrics.checkpoint_registrations, checkpoint_roots);
            assert_eq!(metrics.checkpoint_releases, checkpoint_roots);
            assert_eq!(after.registrations - before.registrations, checkpoint_roots);
            assert_eq!(after.releases - before.releases, checkpoint_roots);
            for node in nodes {
                roots.consume(&machine, node).unwrap();
            }
            assert_eq!(machine.rust_roots_len(), 0);
        }
    }

    #[test]
    fn encoder_root_and_identity_work_stays_bounded_across_wide_arrays() {
        for width in [16_usize, 256, 4096] {
            let (encoded, roots, identity_comparisons, disposition) =
                encode_null_array(width, None, false);
            let encoded = encoded.unwrap();
            assert_eq!(disposition, MachineDisposition::Reusable);
            assert_eq!(encoded.len(), 1 + width * 4 + width.saturating_sub(1) + 1);
            assert!(encoded.starts_with('[') && encoded.ends_with(']'));
            assert_eq!(identity_comparisons, width * 2);
            assert_eq!(roots.registrations, roots.releases);
            assert_eq!(roots.active, 1, "the caller's input root remains owned");
            assert_eq!(roots.peak_active, 6, "width={width}");
            let checkpoint_roots = 1 + (width + 1).ilog2() as usize;
            assert_eq!(roots.registrations, 1 + 2 * width + checkpoint_roots);
        }
    }

    #[test]
    fn encoder_cancellation_releases_the_active_traversal_frontier() {
        let (result, roots, _, disposition) = encode_null_array(4096, Some(2), false);
        assert_eq!(result, Err(CallStatus::Cancelled));
        assert_eq!(disposition, MachineDisposition::Reusable);
        assert_eq!(roots.registrations, roots.releases);
        assert_eq!(roots.active, 1, "only the caller-owned input remains");
        assert_eq!(roots.peak_active, 6);
    }

    #[test]
    fn malformed_managed_pointer_makes_the_encoder_machine_unavailable() {
        let (result, roots, _, disposition) = encode_null_array(1, None, true);
        assert_eq!(result, Err(CallStatus::IntegrityFailure));
        assert_eq!(disposition, MachineDisposition::Unavailable);
        assert_eq!(roots.registrations, roots.releases);
        assert_eq!(roots.active, 1, "only the caller-owned input remains");
    }

    #[test]
    fn temporary_root_slots_grow_geometrically_and_are_reusable() {
        let machine = MachineState::new();
        let mut roots = ConstructionCore::new(1);
        for width in [1_usize, 65, 257] {
            let nodes = rooted_words(&mut roots, &machine, (0..width).map(|index| index * 16));
            assert_eq!(roots.root_metrics().1, width);
            for node in nodes {
                roots.consume(&machine, node).unwrap();
            }
            assert_eq!(roots.root_metrics().1, 0);
            let (free_capacity, admitted_slots) = roots.root_pool_metrics();
            assert!(free_capacity.is_power_of_two());
            assert!(free_capacity >= admitted_slots);
        }
        let metrics = roots.root_operation_metrics();
        assert_eq!(metrics.registrations, metrics.releases);
        assert_eq!(machine.rust_roots_len(), 0);
    }

    #[test]
    fn cycle_identity_index_relocates_witnesses_before_address_reuse() {
        let machine = MachineState::new();
        let mut roots = ConstructionCore::new(1);
        let first = roots
            .push_word(&machine, 0x1001, RuntimeRep::LiftedRef)
            .unwrap();
        let second = roots
            .push_word(&machine, 0x2001, RuntimeRep::LiftedRef)
            .unwrap();
        let mut identities = MovingIdentities {
            generation: 0,
            nodes: HashMap::from([(0x1000, first), (0x2000, second)]),
        };
        // Model the collector updating stable root slots while another object
        // moves into an address previously used by the active traversal.
        unsafe {
            roots.slot(first).unwrap().write(0x3001);
            roots.slot(second).unwrap().write(0x1001);
        }
        identities
            .refresh(1, |node| {
                roots.word(node).map_err(|_| RuntimeError::BadPointer)
            })
            .unwrap();
        assert_eq!(identities.nodes.get(&0x3000), Some(&first));
        assert_eq!(identities.nodes.get(&0x1000), Some(&second));
        assert!(!identities.nodes.contains_key(&0x2000));
        roots.release(&machine);
        assert_eq!(machine.rust_roots_len(), 0);
    }

    #[test]
    fn polling_reader_observes_cancellation_without_heap_pressure() {
        let machine = MachineState::new();
        machine.fail_prepared_at(
            crate::prepared_control::PreparedSafepoint::Backedge,
            2,
            RuntimeError::Cancelled,
        );
        let input = vec![b' '; 32 * 1024];
        let mut reader = PollingReader {
            input: &input,
            offset: 0,
            machine: &machine,
        };
        let mut output = Vec::new();
        assert!(reader.read_to_end(&mut output).is_err());
        assert_eq!(machine.prepared_call_status(), CallStatus::Cancelled);
        assert!(output.len() < input.len());
    }
}
