use crate::emit::expr::{demand_whnf_ssaval, ensure_heap_ptr};
use crate::emit::*;
use cranelift_codegen::ir::{
    self, condcodes::IntCC, types, BlockArg, InstBuilder, MemFlags, Value,
};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};
use tidepool_repr::{Alt, AltCon, Literal, VarId};

/// Emit Case dispatch. The scrutinee has already been evaluated (stack-safe).
pub fn emit_case(
    args: EmitArgs,
    scrut: SsaVal,
    binder: &VarId,
    alts: &[Alt<usize>],
) -> Result<SsaVal, EmitError> {
    let scrut = demand_whnf_ssaval(args.sess.pipeline, args.builder, args.sess.vmctx, scrut)?;

    // Bind the case binder, saving the old value for restore below. EnvGuard
    // can't be used here because it would borrow ctx.env mutably, preventing
    // the use of ctx in subsequent emit_* calls.
    let old_case_binder = args.ctx.env.insert(*binder, scrut);

    let data_alts: Vec<_> = alts
        .iter()
        .filter(|alt| matches!(alt.con, AltCon::DataAlt(_)))
        .collect();
    let lit_alts: Vec<_> = alts
        .iter()
        .filter(|alt| matches!(alt.con, AltCon::LitAlt(_)))
        .collect();
    let default_alt = alts.iter().find(|alt| matches!(alt.con, AltCon::Default));

    let merge_block = args.builder.create_block();
    args.builder.append_block_param(merge_block, types::I64);

    if !data_alts.is_empty() {
        // Data dispatch reads heap headers, including its bare-literal wrapper
        // tolerance. Raw SSA numerics are values, never heap addresses.
        let scrut_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            scrut,
        );
        emit_data_dispatch(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            scrut_ptr,
            &data_alts,
            default_alt,
            merge_block,
        )?;
    } else if !lit_alts.is_empty() {
        emit_lit_dispatch(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            scrut,
            &lit_alts,
            default_alt,
            merge_block,
        )?;
    } else if let Some(alt) = default_alt {
        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);
    } else {
        // No data/lit/default alt at all, so the scrutinee's shape is
        // unknown here — see `trap_scrut_ptr` for why it can't just pass
        // `scrut_ptr`.
        let trap_ptr = trap_scrut_ptr(args.builder, scrut);
        emit_case_trap(
            args.sess,
            args.builder,
            &args.ctx.current_fn,
            trap_ptr,
            &[],
            merge_block,
        )?;
    }

    args.builder.seal_block(merge_block);

    args.builder.switch_to_block(merge_block);
    let result = args.builder.block_params(merge_block)[0];
    args.builder.declare_value_needs_stack_map(result);

    args.ctx.env.restore(*binder, old_case_binder);

    Ok(SsaVal::HeapPtr(result))
}

fn emit_data_dispatch(
    args: EmitArgs,
    initial_scrut_ptr: Value,
    data_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    let scrut_ptr = initial_scrut_ptr;

    let con_tag =
        args.builder
            .ins()
            .load(types::I64, MemFlags::trusted(), scrut_ptr, CON_TAG_OFFSET);

    // Runtime Lit-tolerance: a literal materialized on the Rust side can
    // reach a data case on a boxed-literal wrapper constructor (I#/W#/C#/F#/
    // D#) as a *bare* Lit heap object, not a boxed Con. Its (garbage) con_tag
    // matches no alt, so the chain below would fall through to the trap.
    // Detect the (at most one) wrapper alt at emit time \u2014 zero cost for
    // ordinary ADT cases \u2014 and, when the scrutinee is a Lit at runtime, route
    // to that alt. The wrapper alt's single binder ends up bound to a
    // pointer-to-Lit in BOTH paths (the con path loads field0, which is
    // itself a pointer to a Lit; the Lit path uses the whole scrutinee), so
    // the body's downstream unboxing sees an identical representation. The
    // alt block is given a binder parameter so both paths share one emitted
    // body.
    let wrapper_pos = data_alts.iter().position(
        |alt| matches!(&alt.con, AltCon::DataAlt(tag) if args.sess.lit_wrappers.is_wrapper(*tag)),
    );

    let wrapper_block = wrapper_pos.map(|_| {
        let b = args.builder.create_block();
        args.builder.append_block_param(b, types::I64);
        b
    });

    if let Some(wb) = wrapper_block {
        let kind_tag = args
            .builder
            .ins()
            .load(types::I8, MemFlags::trusted(), scrut_ptr, 0);
        let is_lit = args
            .builder
            .ins()
            .icmp_imm(IntCC::Equal, kind_tag, TAG_LIT as i64);
        let con_path_block = args.builder.create_block();
        args.builder.ins().brif(
            is_lit,
            wb,
            &[BlockArg::Value(scrut_ptr)],
            con_path_block,
            &[],
        );
        args.builder.switch_to_block(con_path_block);
        args.builder.seal_block(con_path_block);
    }

    // Use comparison chain instead of jump table because DataConIds are large
    // GHC Uniques (arbitrary u64 values), not small sequential integers.
    for (alt_idx, &alt) in data_alts.iter().enumerate() {
        let AltCon::DataAlt(tag) = &alt.con else {
            continue;
        };
        let is_wrapper = Some(alt_idx) == wrapper_pos;

        let alt_block = if is_wrapper {
            #[allow(
                clippy::expect_used,
                reason = "wrapper_block is Some whenever wrapper_pos is Some"
            )]
            wrapper_block.expect("wrapper_block is Some whenever wrapper_pos is Some")
        } else {
            args.builder.create_block()
        };
        let next_check_block = args.builder.create_block();

        let tag_val = args.builder.ins().iconst(types::I64, tag.0 as i64);
        let eq = args.builder.ins().icmp(IntCC::Equal, con_tag, tag_val);
        if is_wrapper {
            // Con path: the wrapper has exactly one field \u2014 a pointer to a Lit.
            // Pass it as the shared binder parameter (matching the Lit path,
            // which passes the scrutinee Lit itself).
            let field0 = args.builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                scrut_ptr,
                CON_FIELDS_OFFSET,
            );
            args.builder.declare_value_needs_stack_map(field0);
            args.builder.ins().brif(
                eq,
                alt_block,
                &[BlockArg::Value(field0)],
                next_check_block,
                &[],
            );
        } else {
            args.builder
                .ins()
                .brif(eq, alt_block, &[], next_check_block, &[]);
        }

        args.builder.switch_to_block(alt_block);
        // For a wrapper alt block both predecessors (the Lit branch above and
        // the con-tag branch just emitted) are now wired, so sealing is safe.
        args.builder.seal_block(alt_block);

        // Bind pattern variables \u2014 do NOT force thunked fields.
        // In Haskell, case alt binders are lazy. Thunked Con fields
        // remain as thunks until used in a strict context (case scrutiny,
        // primop args, etc.). Forcing here causes infinite loops for
        // self-referencing structures like `xs = 1 : map (+1) xs`.
        //
        // Strict consumers demand these fields at their owning boundary;
        // matching a constructor must not demand its unselected fields.
        let mut scope = EnvScope::new();
        // NOTE: EnvGuard cannot be used here because it would borrow ctx.env
        // mutably, preventing the use of ctx in emit_node.
        if is_wrapper {
            // Single binder bound to the binder parameter (a pointer to a Lit).
            let binder_val = args.builder.block_params(alt_block)[0];
            args.builder.declare_value_needs_stack_map(binder_val);
            if let Some(&binder) = alt.binders.first() {
                args.ctx
                    .env
                    .insert_scoped(&mut scope, binder, SsaVal::HeapPtr(binder_val));
            }
        } else {
            for (i, &binder) in alt.binders.iter().enumerate() {
                let offset = CON_FIELDS_OFFSET + (8 * i as i32);
                let field_val =
                    args.builder
                        .ins()
                        .load(types::I64, MemFlags::trusted(), scrut_ptr, offset);
                args.builder.declare_value_needs_stack_map(field_val);
                args.ctx
                    .env
                    .insert_scoped(&mut scope, binder, SsaVal::HeapPtr(field_val));
            }
        }

        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);

        args.ctx.env.restore_scope(scope);

        args.builder.switch_to_block(next_check_block);
        args.builder.seal_block(next_check_block);
    }

    if let Some(alt) = default_alt {
        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);
    } else {
        emit_case_trap(
            args.sess,
            args.builder,
            &args.ctx.current_fn,
            scrut_ptr,
            data_alts,
            merge_block,
        )?;
    }

    Ok(())
}

/// The `scrut_ptr` arg `runtime_shape_trap` dereferences before its own
/// null/validity check (M5): only ever pass an actual heap pointer. An
/// unboxed `Raw` scrutinee (int/float bits, not an address) becomes 0
/// instead, which the trap already treats as safely absent.
fn trap_scrut_ptr(builder: &mut FunctionBuilder, scrut: SsaVal) -> Value {
    match scrut {
        SsaVal::HeapPtr(ptr) => ptr,
        SsaVal::Raw(_, _) => builder.ins().iconst(types::I64, 0),
    }
}

/// Emit a call to `runtime_shape_trap` (kind `CaseMiss`) instead of a bare
/// `trap user2`. Passes the scrutinee pointer and expected alt tags for
/// diagnostic output.
fn emit_case_trap(
    sess: &mut EmitSession,
    builder: &mut FunctionBuilder,
    fn_name: &str,
    scrut_ptr: Value,
    data_alts: &[&Alt<usize>],
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // Intern the enclosing-function name in the pipeline-owned arena
    // (`CodegenPipeline::intern_name`) for the diagnostic: the pointer
    // compiled code embeds must live exactly as long as the pipeline that
    // owns the code referencing it, not `'static` — a long-lived server
    // process compiles many machines, and a real `Box::leak` per case site
    // would be unbounded over the process lifetime. Deduped by name, so N
    // case sites in one function share one allocation.
    let (name_ptr_raw, name_len_raw) = sess.pipeline.intern_name(fn_name);
    let name_ptr = builder.ins().iconst(types::I64, name_ptr_raw as i64);
    let name_len = builder.ins().iconst(types::I64, name_len_raw as i64);
    let tags: Vec<u64> = data_alts
        .iter()
        .filter_map(|alt| {
            if let AltCon::DataAlt(tag) = &alt.con {
                Some(tag.0)
            } else {
                None
            }
        })
        .collect();

    let num_alts = tags.len();
    let ss = builder.create_sized_stack_slot(ir::StackSlotData::new(
        ir::StackSlotKind::ExplicitSlot,
        (num_alts * 8) as u32,
        3, // align 8
    ));
    for (i, &tag) in tags.iter().enumerate() {
        let tag_val = builder.ins().iconst(types::I64, tag as i64);
        builder.ins().stack_store(tag_val, ss, (i * 8) as i32);
    }
    let tags_addr = builder.ins().stack_addr(types::I64, ss, 0);

    let trap_fn = sess
        .pipeline
        .module
        .declare_function(
            "runtime_shape_trap",
            Linkage::Import,
            &crate::emit::runtime_shape_trap_sig(sess.pipeline.isa.default_call_conv()),
        )
        .map_err(|e| EmitError::CraneliftError(e.to_string()))?;
    let trap_ref = sess
        .pipeline
        .module
        .declare_func_in_func(trap_fn, builder.func);
    let kind = builder
        .ins()
        .iconst(types::I64, crate::host_fns::ShapeTrapKind::CaseMiss as i64);
    let num_alts_val = builder.ins().iconst(types::I64, num_alts as i64);
    let call = builder.ins().call(
        trap_ref,
        &[kind, scrut_ptr, num_alts_val, tags_addr, name_ptr, name_len],
    );
    let result = builder.inst_results(call)[0];
    builder.ins().jump(merge_block, &[BlockArg::Value(result)]);
    Ok(())
}

fn emit_lit_dispatch(
    args: EmitArgs,
    scrut: SsaVal,
    lit_alts: &[&Alt<usize>],
    default_alt: Option<&Alt<usize>>,
    merge_block: ir::Block,
) -> Result<(), EmitError> {
    // Unboxing is the owning literal-class check, including boxed wrappers
    // and deferred fields. Never interpret a closure payload as an integer.
    let scrut_value = match &lit_alts[0].con {
        AltCon::LitAlt(Literal::LitFloat(_)) => crate::emit::primop::unbox_float(
            args.sess.pipeline,
            args.builder,
            args.sess.vmctx,
            scrut,
        ),
        AltCon::LitAlt(Literal::LitDouble(_)) => crate::emit::primop::unbox_double(
            args.sess.pipeline,
            args.builder,
            args.sess.vmctx,
            scrut,
        ),
        _ => {
            crate::emit::primop::unbox_int(args.sess.pipeline, args.builder, args.sess.vmctx, scrut)
        }
    };

    for &alt in lit_alts {
        let alt_block = args.builder.create_block();
        let next_check_block = args.builder.create_block();

        if let AltCon::LitAlt(lit) = &alt.con {
            match lit {
                Literal::LitInt(n) => {
                    let lit_val = args.builder.ins().iconst(types::I64, *n);
                    let eq = args.builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitWord(n) => {
                    let lit_val = args.builder.ins().iconst(types::I64, *n as i64);
                    let eq = args.builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitChar(c) => {
                    let lit_val = args.builder.ins().iconst(types::I64, *c as i64);
                    let eq = args.builder.ins().icmp(IntCC::Equal, scrut_value, lit_val);
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitFloat(bits) => {
                    let lit_val = args.builder.ins().f32const(f32::from_bits(*bits as u32));
                    let eq = args.builder.ins().fcmp(
                        ir::condcodes::FloatCC::Equal,
                        scrut_value,
                        lit_val,
                    );
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitDouble(bits) => {
                    let lit_val = args.builder.ins().f64const(f64::from_bits(*bits));
                    let eq = args.builder.ins().fcmp(
                        ir::condcodes::FloatCC::Equal,
                        scrut_value,
                        lit_val,
                    );
                    args.builder
                        .ins()
                        .brif(eq, alt_block, &[], next_check_block, &[]);
                }
                Literal::LitString(_) | Literal::LitByteArray(_) => {
                    return Err(EmitError::NotYetImplemented("LitString in Case".into()))
                }
            }
        }

        args.builder.switch_to_block(alt_block);
        args.builder.seal_block(alt_block);
        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);

        args.builder.switch_to_block(next_check_block);
        args.builder.seal_block(next_check_block);
    }

    if let Some(alt) = default_alt {
        let result = EmitContext::emit_node(
            EmitArgs {
                ctx: args.ctx,
                sess: args.sess,
                builder: args.builder,
                tail: args.tail,
            },
            alt.body,
        )?;
        let result_ptr = ensure_heap_ptr(
            args.builder,
            args.sess.vmctx,
            args.sess.gc_sig,
            args.sess.oom_func,
            result,
        );
        args.builder
            .ins()
            .jump(merge_block, &[BlockArg::Value(result_ptr)]);
    } else {
        // `scrut_value` is the UNBOXED literal (int/float bits), not a
        // pointer, so pass `scrut` (not `scrut_value`) through
        // `trap_scrut_ptr` to get a real pointer or a safe 0.
        let trap_ptr = trap_scrut_ptr(args.builder, scrut);
        emit_case_trap(
            args.sess,
            args.builder,
            &args.ctx.current_fn,
            trap_ptr,
            &[],
            merge_block,
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::jit_machine::JitEffectMachine;
    use tidepool_repr::{Alt, AltCon, CoreFrame, DataConTable, Literal, TreeBuilder, VarId};

    #[test]
    fn strict_demand_data_dispatch_boxes_raw_numeric_scrutinee() {
        let mut table = DataConTable::new();
        let wrapper = tidepool_repr::DataConId(1);
        table.insert(tidepool_repr::datacon::DataCon {
            id: wrapper,
            name: "I#".into(),
            tag: 1,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
            type_name: String::new(),
        });
        let mut b = TreeBuilder::new();
        let value = b.push(CoreFrame::Lit(Literal::LitInt(42)));
        let field = b.push(CoreFrame::Var(VarId(2)));
        b.push(CoreFrame::Case {
            scrutinee: value,
            binder: VarId(1),
            alts: vec![Alt {
                con: AltCon::DataAlt(wrapper),
                binders: vec![VarId(2)],
                body: field,
            }],
        });
        let mut machine = JitEffectMachine::compile(&b.build(), &table, 65536).unwrap();
        assert!(matches!(
            machine.run_pure().unwrap(),
            tidepool_eval::Value::Lit(Literal::LitInt(42))
        ));
    }

    #[test]
    fn strict_demand_case_bottom_never_executes_default() {
        for literal_arm in [false, true] {
            let mut b = TreeBuilder::new();
            let bottom = b.push(CoreFrame::Var(VarId(0x4500_0000_0000_0003)));
            let value = b.push(CoreFrame::Lit(Literal::LitInt(42)));
            let mut alts = vec![Alt {
                con: AltCon::Default,
                binders: vec![],
                body: value,
            }];
            if literal_arm {
                alts.push(Alt {
                    con: AltCon::LitAlt(Literal::LitInt(0)),
                    binders: vec![],
                    body: value,
                });
            }
            b.push(CoreFrame::Case {
                scrutinee: bottom,
                binder: VarId(1),
                alts,
            });
            let mut machine =
                JitEffectMachine::compile(&b.build(), &DataConTable::new(), 65536).unwrap();
            let result = machine.run_pure();
            assert!(result.is_err(), "bottom escaped through case: {result:?}");
            assert!(matches!(
                result,
                Err(crate::jit_machine::JitError::Yield(
                    crate::yield_type::YieldError::Runtime(
                        crate::host_fns::RuntimeError::Undefined
                    )
                ))
            ));
        }
    }

    #[test]
    fn strict_demand_literal_dispatch_uses_float_width() {
        for (scrutinee, matching) in [
            (
                Literal::LitFloat(1.5f32.to_bits() as u64),
                Literal::LitFloat(1.5f32.to_bits() as u64),
            ),
            (
                Literal::LitDouble(1.5f64.to_bits()),
                Literal::LitDouble(1.5f64.to_bits()),
            ),
        ] {
            let mut b = TreeBuilder::new();
            let scrutinee = b.push(CoreFrame::Lit(scrutinee));
            let yes = b.push(CoreFrame::Lit(Literal::LitInt(42)));
            let no = b.push(CoreFrame::Lit(Literal::LitInt(0)));
            b.push(CoreFrame::Case {
                scrutinee,
                binder: VarId(1),
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(matching),
                        binders: vec![],
                        body: yes,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: no,
                    },
                ],
            });
            let mut machine =
                JitEffectMachine::compile(&b.build(), &DataConTable::new(), 65536).unwrap();
            assert!(matches!(
                machine.run_pure().unwrap(),
                tidepool_eval::Value::Lit(Literal::LitInt(42))
            ));
        }
    }
}
