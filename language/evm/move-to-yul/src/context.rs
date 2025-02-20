// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{
    attributes, evm_transformation::EvmTransformationProcessor, native_functions::NativeFunctions,
    yul_functions, yul_functions::YulFunction, Options,
};
use itertools::Itertools;
use move_model::{
    ast::TempIndex,
    code_writer::CodeWriter,
    emitln,
    model::{FunId, FunctionEnv, GlobalEnv, ModuleEnv, QualifiedId, QualifiedInstId, StructId},
    ty::{PrimitiveType, Type},
};
use move_stackless_bytecode::{
    function_target::FunctionTarget,
    function_target_pipeline::{FunctionTargetPipeline, FunctionTargetsHolder, FunctionVariant},
    livevar_analysis::LiveVarAnalysisProcessor,
    reaching_def_analysis::ReachingDefProcessor,
};
use std::{cell::RefCell, collections::BTreeMap};

/// Immutable context passed through the compilation.
pub(crate) struct Context<'a> {
    /// The program options.
    pub options: &'a Options,
    /// The global environment, containing the Move model.
    pub env: &'a GlobalEnv,
    /// The function target data containing the stackless bytecode.
    pub targets: FunctionTargetsHolder,
    /// A code writer where we emit Yul code to.
    pub writer: CodeWriter,
    /// Cached memory layout info.
    pub struct_layout: RefCell<BTreeMap<QualifiedInstId<StructId>, StructLayout>>,
    /// Native function info.
    pub native_funs: NativeFunctions,
}

/// Information about the layout of a struct in linear memory.
#[derive(Default, Clone)]
pub(crate) struct StructLayout {
    /// The size, in bytes, of this struct.
    pub size: usize,
    /// Offsets in linear memory and type for each field, indexed by logical offsets, i.e.
    /// position in the struct definition.
    pub offsets: BTreeMap<usize, (usize, Type)>,
    /// Field order (in terms of logical offset), optimized for best memory representation.
    pub field_order: Vec<usize>,
    /// The number of leading fields which are pointers to linked data. Those fields always
    /// appear first in the field_order.
    pub pointer_count: usize,
}

impl<'a> Context<'a> {
    /// Create a new context.
    pub fn new(options: &'a Options, env: &'a GlobalEnv, for_test: bool) -> Self {
        let writer = CodeWriter::new(env.unknown_loc());
        writer.set_emit_hook(yul_functions::substitute_placeholders);
        let mut ctx = Self {
            options,
            env,
            targets: Self::create_bytecode(options, env, for_test),
            writer,
            struct_layout: Default::default(),
            native_funs: NativeFunctions::default(),
        };
        ctx.native_funs = NativeFunctions::create(&ctx);
        ctx
    }

    /// Helper to create the stackless bytecode.
    fn create_bytecode(
        options: &Options,
        env: &GlobalEnv,
        for_test: bool,
    ) -> FunctionTargetsHolder {
        // Populate the targets holder with all needed functions.
        let mut targets = FunctionTargetsHolder::default();
        let is_used_fun = |fun: &FunctionEnv| {
            if for_test {
                attributes::is_test_fun(fun)
            } else {
                attributes::is_callable_fun(fun)
                    || attributes::is_create_fun(fun)
                    || attributes::is_receive_fun(fun)
                    || attributes::is_fallback_fun(fun)
            }
        };
        for module in env.get_modules() {
            if !module.is_target() {
                continue;
            }
            for fun in module.get_functions() {
                if is_used_fun(&fun) {
                    Self::add_fun(&mut targets, &fun)
                }
            }
        }
        // Run a minimal transformation pipeline. For now, we do some evm pre-processing,
        // and reaching-def and live-var to clean up some churn created by the conversion from
        // stack to stackless bytecode.
        let mut pipeline = FunctionTargetPipeline::default();
        pipeline.add_processor(EvmTransformationProcessor::new());
        pipeline.add_processor(ReachingDefProcessor::new());
        pipeline.add_processor(LiveVarAnalysisProcessor::new());
        if options.dump_bytecode {
            pipeline.run_with_dump(env, &mut targets, &options.output, false)
        } else {
            pipeline.run(env, &mut targets);
        }

        targets
    }

    /// Adds function and all its called functions to the targets.
    fn add_fun(targets: &mut FunctionTargetsHolder, fun: &FunctionEnv<'_>) {
        targets.add_target(fun);
        for qid in fun.get_called_functions() {
            let called_fun = fun.module_env.env.get_function(qid);
            if !targets.has_target(&called_fun, &FunctionVariant::Baseline) {
                Self::add_fun(targets, &called_fun)
            }
        }
    }

    /// Return iterator for all functions in the environment which stem from a target module
    /// and which satsify predicate.
    pub fn get_target_functions(&self, p: impl Fn(&FunctionEnv) -> bool) -> Vec<FunctionEnv<'a>> {
        self.env
            .get_modules()
            .filter(|m| m.is_target())
            .map(|m| m.into_functions().filter(|f| p(f)))
            .flatten()
            .collect()
    }

    /// Check whether given Move function has no generics; report error otherwise.
    pub fn check_no_generics(&self, fun: &FunctionEnv<'_>) {
        if fun.get_type_parameter_count() > 0 {
            self.env.error(
                &fun.get_loc(),
                "#[callable] or #[create] functions cannot be generic",
            )
        }
    }

    /// Make the name of a contract.
    pub fn make_contract_name(&self, module: &ModuleEnv) -> String {
        let mod_name = module.get_name();
        let mod_sym = module.symbol_pool().string(mod_name.name());
        format!("A{}_{}", mod_name.addr().to_str_radix(16), mod_sym)
    }

    /// Make the name of function.
    pub fn make_function_name(&self, fun_id: &QualifiedInstId<FunId>) -> String {
        let fun = self.env.get_function(fun_id.to_qualified_id());
        let fun_sym = fun.symbol_pool().string(fun.get_name());
        format!(
            "{}_{}{}",
            self.make_contract_name(&fun.module_env),
            fun_sym,
            self.mangle_types(&fun_id.inst)
        )
    }

    /// Mangle a type for being part of name.
    ///
    /// Note that the mangled type representation is also used to create a hash for types
    /// in `Generator::type_hash` which is used to index storage. Therefore the representation here
    /// cannot be changed without creating versioning problems for existing storage of contracts.
    pub fn mangle_type(&self, ty: &Type) -> String {
        use move_model::ty::{PrimitiveType::*, Type::*};
        match ty {
            Primitive(p) => match p {
                U8 => "u8".to_string(),
                U64 => "u64".to_string(),
                U128 => "u128".to_string(),
                Num => "num".to_string(),
                Address => "address".to_string(),
                Signer => "signer".to_string(),
                Bool => "bool".to_string(),
                Range => "range".to_string(),
                _ => format!("<<unsupported {:?}>>", ty),
            },
            Vector(et) => format!("vec{}", self.mangle_types(&[et.as_ref().to_owned()])),
            Struct(mid, sid, inst) => {
                self.mangle_struct(&mid.qualified(*sid).instantiate(inst.clone()))
            }
            TypeParameter(..) | Fun(..) | Tuple(..) | TypeDomain(..) | ResourceDomain(..)
            | Error | Var(..) | Reference(..) => format!("<<unsupported {:?}>>", ty),
        }
    }

    /// Mangle a struct.
    fn mangle_struct(&self, struct_id: &QualifiedInstId<StructId>) -> String {
        let struct_env = &self.env.get_struct(struct_id.to_qualified_id());
        let module_name = self.make_contract_name(&struct_env.module_env);
        format!(
            "{}_{}{}",
            module_name,
            struct_env.get_name().display(struct_env.symbol_pool()),
            self.mangle_types(&struct_id.inst)
        )
    }

    /// Mangle a slice of types.
    pub fn mangle_types(&self, tys: &[Type]) -> String {
        if tys.is_empty() {
            "".to_owned()
        } else {
            format!("${}$", tys.iter().map(|ty| self.mangle_type(ty)).join("_"))
        }
    }

    /// Make name for a local.
    pub fn make_local_name(&self, target: &FunctionTarget, idx: TempIndex) -> String {
        target
            .get_local_name(idx)
            .display(target.symbol_pool())
            .to_string()
            .replace("#", "_")
    }

    /// Make name for a result.
    pub fn make_result_name(&self, target: &FunctionTarget, idx: usize) -> String {
        if target.get_return_count() == 1 {
            "$result".to_string()
        } else {
            format!("$result{}", idx)
        }
    }

    /// Emits a Yul block.
    pub fn emit_block(&self, blk: impl FnOnce()) {
        emitln!(self.writer, "{");
        self.writer.indent();
        blk();
        self.writer.unindent();
        emitln!(self.writer, "}");
    }

    /// Get the field types of a struct as a vector.
    pub fn get_field_types(&self, id: QualifiedId<StructId>) -> Vec<Type> {
        self.env
            .get_struct(id)
            .get_fields()
            .map(|f| f.get_type())
            .collect()
    }

    /// Returns whether the struct identified by module_id and struct_id is the native U256 struct.
    pub fn is_u256(&self, struct_id: QualifiedId<StructId>) -> bool {
        let struct_env = self.env.get_struct(struct_id);
        attributes::is_evm_arith_module(&struct_env.module_env) && struct_env.is_native()
    }

    /// Check whether ty is a static type in the sense of serialization
    pub fn abi_is_static_type(&self, ty: &Type) -> bool {
        use move_model::ty::{PrimitiveType::*, Type::*};

        let conjunction = |tys: &[Type]| {
            tys.iter()
                .map(|t| self.abi_is_static_type(t))
                .collect::<Vec<_>>()
                .into_iter()
                .all(|t| t)
        };
        match ty {
            Primitive(p) => match p {
                Bool | U8 | U64 | U128 | Address | Signer => true,
                _ => {
                    panic!("unexpected field type")
                }
            },
            Vector(_) => false,
            Tuple(tys) => conjunction(tys),
            Struct(mid, sid, _) => {
                if self.is_u256(mid.qualified(*sid)) {
                    true
                } else {
                    let tys = self.get_field_types(mid.qualified(*sid));
                    conjunction(&tys)
                }
            }
            TypeParameter(_)
            | Reference(_, _)
            | Fun(_, _)
            | TypeDomain(_)
            | ResourceDomain(_, _, _)
            | Error
            | Var(_) => {
                panic!("unexpected field type")
            }
        }
    }

    /// Compute the sum of data size of tys
    pub fn abi_type_head_sizes_sum(&self, tys: &[Type], padded: bool) -> usize {
        let size_vec = self.abi_type_head_sizes_vec(tys, padded);
        size_vec.iter().map(|(_, size)| size).sum()
    }

    /// Compute the data size of all types in tys
    pub fn abi_type_head_sizes_vec(&self, tys: &[Type], padded: bool) -> Vec<(Type, usize)> {
        tys.iter()
            .map(|ty_| (ty_.clone(), self.abi_type_head_size(ty_, padded)))
            .collect_vec()
    }

    /// Compute the data size of ty on the stack
    pub fn abi_type_head_size(&self, ty: &Type, padded: bool) -> usize {
        use move_model::ty::{PrimitiveType::*, Type::*};
        if self.abi_is_static_type(ty) {
            match ty {
                Primitive(p) => match p {
                    Bool => {
                        if padded {
                            32
                        } else {
                            1
                        }
                    }
                    U8 => {
                        if padded {
                            32
                        } else {
                            1
                        }
                    }
                    U64 => {
                        if padded {
                            32
                        } else {
                            8
                        }
                    }
                    U128 => {
                        if padded {
                            32
                        } else {
                            16
                        }
                    }
                    Address | Signer => {
                        if padded {
                            32
                        } else {
                            20
                        }
                    }
                    Num | Range | EventStore => {
                        panic!("unexpected field type")
                    }
                },
                Tuple(tys) => self.abi_type_head_sizes_sum(tys, padded),
                Struct(mid, sid, _) => {
                    if self.is_u256(mid.qualified(*sid)) {
                        32
                    } else {
                        let tys = self.get_field_types(mid.qualified(*sid));
                        self.abi_type_head_sizes_sum(&tys, padded)
                    }
                }
                _ => panic!("unexpected field type"),
            }
        } else {
            // Dynamic types
            32
        }
    }

    /// Get the layout of the instantiated struct in linear memory. The result will be cached
    /// for future calls.
    pub fn get_struct_layout(&self, st: &QualifiedInstId<StructId>) -> StructLayout {
        let mut layouts_ref = self.struct_layout.borrow_mut();
        if layouts_ref.get(st).is_none() {
            // Compute the fields such that the larger appear first, and pointer fields
            // precede non-pointer fields.
            let s_or_v = |ty: &Type| ty.is_vector() || ty.is_struct();
            let struct_env = self.env.get_struct(st.to_qualified_id());
            let ordered_fields = struct_env
                .get_fields()
                .map(|field| {
                    let field_type = field.get_type().instantiate(&st.inst);
                    let field_size = self.type_size(&field_type);
                    (field.get_offset(), field_size, field_type)
                })
                .sorted_by(|(_, s1, ty1), (_, s2, ty2)| {
                    if s1 > s2 {
                        std::cmp::Ordering::Less
                    } else if s2 > s1 {
                        std::cmp::Ordering::Greater
                    } else if s_or_v(ty1) && !s_or_v(ty2) {
                        std::cmp::Ordering::Less
                    } else if s_or_v(ty2) && !s_or_v(ty1) {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Equal
                    }
                });
            let mut result = StructLayout::default();
            for (logical_offs, field_size, ty) in ordered_fields {
                result.field_order.push(logical_offs);
                if s_or_v(&ty) {
                    result.pointer_count += 1
                }
                result.offsets.insert(logical_offs, (result.size, ty));
                result.size += field_size
            }
            layouts_ref.insert(st.clone(), result);
        }
        layouts_ref.get(st).unwrap().clone()
    }

    /// Calculate the size, in bytes, for the memory layout of this type.
    pub fn type_size(&self, ty: &Type) -> usize {
        use PrimitiveType::*;
        use Type::*;
        match ty {
            Primitive(p) => match p {
                Bool | U8 => 1,
                U64 => 8,
                U128 => 16,
                Address | Signer => 20,
                Num | Range | EventStore => {
                    panic!("unexpected field type")
                }
            },
            Struct(..) | Vector(..) => 32,
            Tuple(_)
            | TypeParameter(_)
            | Reference(_, _)
            | Fun(_, _)
            | TypeDomain(_)
            | ResourceDomain(_, _, _)
            | Error
            | Var(_) => {
                panic!("unexpected field type")
            }
        }
    }

    /// Returns the max value (bit mask) for a given type.
    pub fn max_value(&self, ty: &Type) -> String {
        let size = self.type_size(ty.skip_reference());
        match size {
            1 => "${MAX_U8}".to_string(),
            8 => "${MAX_U64}".to_string(),
            16 => "${MAX_U128}".to_string(),
            20 => "${ADDRESS_U160}".to_string(),
            32 => "${MAX_U256}".to_string(),
            _ if self.type_allocates_memory(ty) => {
                // Type allocates a pointer which uses 256 bits
                "${MAX_U256}".to_string()
            }
            _ => panic!(
                "unexpected type size {} for `{}`",
                size,
                ty.display(&self.env.get_type_display_ctx())
            ),
        }
    }

    /// Returns the Load function for a given type.
    pub fn load_builtin_fun(&self, ty: &Type) -> YulFunction {
        match self.type_size(ty.skip_reference()) {
            1 => YulFunction::LoadU8,
            8 => YulFunction::LoadU64,
            16 => YulFunction::LoadU128,
            32 => YulFunction::LoadU256,
            _ => panic!("unexpected type size"),
        }
    }

    /// Returns the Store function for a given type.
    pub fn store_builtin_fun(&self, ty: &Type) -> YulFunction {
        match self.type_size(ty.skip_reference()) {
            1 => YulFunction::StoreU8,
            8 => YulFunction::StoreU64,
            16 => YulFunction::StoreU128,
            32 => YulFunction::StoreU256,
            _ => panic!("unexpected type size"),
        }
    }

    /// Returns the MemoryLoad function for a given type.
    pub fn memory_load_builtin_fun(&self, ty: &Type) -> YulFunction {
        match self.type_size(ty.skip_reference()) {
            1 => YulFunction::MemoryLoadU8,
            8 => YulFunction::MemoryLoadU64,
            16 => YulFunction::MemoryLoadU128,
            32 => YulFunction::MemoryLoadU256,
            _ => panic!("unexpected type size"),
        }
    }

    /// Returns the MemoryStore function for a given type.
    pub fn memory_store_builtin_fun(&self, ty: &Type) -> YulFunction {
        match self.type_size(ty.skip_reference()) {
            1 => YulFunction::MemoryStoreU8,
            8 => YulFunction::MemoryStoreU64,
            16 => YulFunction::MemoryStoreU128,
            32 => YulFunction::MemoryStoreU256,
            _ => panic!("unexpected type size"),
        }
    }

    /// Returns the StorageLoad function for a given type.
    #[allow(dead_code)]
    pub fn storage_load_builtin_fun(&self, ty: &Type) -> YulFunction {
        match self.type_size(ty.skip_reference()) {
            1 => YulFunction::StorageLoadU8,
            8 => YulFunction::StorageLoadU64,
            16 => YulFunction::StorageLoadU128,
            32 => YulFunction::StorageLoadU256,
            _ => panic!("unexpected type size"),
        }
    }

    /// Returns the StorageStore function for a given type.
    #[allow(dead_code)]
    pub fn storage_store_builtin_fun(&self, ty: &Type) -> YulFunction {
        match self.type_size(ty.skip_reference()) {
            1 => YulFunction::StorageStoreU8,
            8 => YulFunction::StorageStoreU64,
            16 => YulFunction::StorageStoreU128,
            32 => YulFunction::StorageStoreU256,
            _ => panic!("unexpected type size"),
        }
    }

    /// Returns true of the type allocates memory.
    pub fn type_allocates_memory(&self, ty: &Type) -> bool {
        use Type::*;
        match ty {
            Struct(m, s, _) => !self.is_u256(m.qualified(*s)),
            Vector(_) => true,
            _ => false,
        }
    }
}
