//! The llvm-sys walk. This is the only module in the crate containing
//! `unsafe`, and no LLVM handle leaves it: every public value is owned Rust
//! data.

use std::{
    collections::{BTreeSet, HashMap},
    ffi::{CStr, c_char, c_void},
    path::PathBuf,
};

use llvm_sys::{
    LLVMDiagnosticSeverity, LLVMLinkage, LLVMOpcode,
    bit_reader::LLVMParseBitcodeInContext2,
    core::*,
    debuginfo::{
        LLVMDIFileGetDirectory, LLVMDIFileGetFilename, LLVMDILocationGetColumn,
        LLVMDILocationGetInlinedAt, LLVMDILocationGetLine, LLVMDILocationGetScope,
        LLVMDIScopeGetFile, LLVMInstructionGetDebugLoc,
    },
    prelude::*,
};

use crate::{
    error::Error,
    query::{facts::*, load::LoadedModule},
};

/// Version of the LLVM this binary links, from the C API.
pub fn llvm_version() -> String {
    let (mut major, mut minor, mut patch) = (0, 0, 0);
    // SAFETY: LLVMGetVersion writes three unsigned values and nothing else.
    unsafe { llvm_sys::core::LLVMGetVersion(&mut major, &mut minor, &mut patch) };
    format!("{major}.{minor}.{patch}")
}

#[derive(Clone, Debug, Default)]
pub struct ModuleFacts {
    pub functions: Vec<FunctionFact>,
    pub call_sites: Vec<CallSiteFact>,
    pub uses: Vec<UseFact>,
    /// Non-fatal problems observed while extracting. Surfaced in the
    /// analysis report, not swallowed into logging.
    pub diagnostics: Vec<String>,
}

/// Disposes the context on every exit path, including the early returns a
/// failed parse takes: a returned `Err` must not leak it.
struct Context(LLVMContextRef);

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `LLVMContextCreate` in `extract_inner`
        // and is disposed exactly once, after every module in it is disposed.
        unsafe { LLVMContextDispose(self.0) };
    }
}

/// Declared after [`Context`] so it is dropped first: a module must not
/// outlive the context that owns it.
struct ParsedModule(LLVMModuleRef);

impl Drop for ParsedModule {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a successful
        // `LLVMParseBitcodeInContext2` and is disposed exactly once.
        unsafe { LLVMDisposeModule(self.0) };
    }
}

/// `LLVMParseBitcodeInContext2` reads the buffer without taking ownership, so
/// the buffer is disposed here rather than by the parse.
struct Buffer(LLVMMemoryBufferRef);

impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: the pointer came from
        // `LLVMCreateMemoryBufferWithMemoryRange` and is disposed exactly
        // once. Disposal frees the buffer object, never the borrowed range.
        unsafe { LLVMDisposeMemoryBuffer(self.0) };
    }
}

/// Owns the buffer LLVM's diagnostic handler writes into. It is held behind a
/// raw pointer so the handler and this side share one provenance, and it is
/// freed in `Drop` on every exit path.
struct DiagnosticSink(*mut Vec<String>);

impl DiagnosticSink {
    fn new() -> Self {
        Self(Box::into_raw(Box::new(Vec::new())))
    }

    /// Takes what has been collected so far. LLVM calls diagnostic handlers
    /// synchronously from the calls this module makes, so no handler can be
    /// running concurrently with this.
    fn take(&self) -> Vec<String> {
        // SAFETY: the pointer came from `Box::into_raw` in `new` and is
        // reclaimed only in `Drop`, so it is valid and uniquely owned here.
        unsafe { std::mem::take(&mut *self.0) }
    }
}

impl Drop for DiagnosticSink {
    fn drop(&mut self) {
        // SAFETY: reclaims the box `new` leaked, exactly once.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

/// Collects LLVM's diagnostics instead of leaving the default handler in
/// place, which prints an error and calls `exit(1)`. A module LLVM refuses
/// must become an `Err`, never a terminated process.
extern "C" fn collect_diagnostic(info: LLVMDiagnosticInfoRef, context: *mut c_void) {
    if info.is_null() || context.is_null() {
        return;
    }
    // SAFETY: `context` is the pointer `DiagnosticSink::new` leaked from a
    // `Box<Vec<String>>`, installed on the context that raised this
    // diagnostic. LLVM calls handlers synchronously on the calling thread and
    // the sink outlives that context, so this is the only live reference.
    let sink = unsafe { &mut *context.cast::<Vec<String>>() };
    // SAFETY: `info` is the live diagnostic LLVM passed in, and
    // `LLVMGetDiagInfoDescription` returns a message the caller owns.
    let description = unsafe { owned_message(LLVMGetDiagInfoDescription(info)) };
    // SAFETY: `info` is the live diagnostic LLVM passed in.
    let severity = match unsafe { LLVMGetDiagInfoSeverity(info) } {
        LLVMDiagnosticSeverity::LLVMDSError => "error",
        LLVMDiagnosticSeverity::LLVMDSWarning => "warning",
        LLVMDiagnosticSeverity::LLVMDSRemark => "remark",
        LLVMDiagnosticSeverity::LLVMDSNote => "note",
    };
    sink.push(format!("{severity}: {description}"));
}

/// Copies a length-delimited buffer LLVM owns into an owned `String`.
///
/// The `LLVMGet*Filename`/`Directory` family writes a length through an out
/// parameter and the buffer is not guaranteed NUL-terminated, so the length
/// must be honoured.
///
/// # Safety
/// `pointer` must be valid for `length` bytes and stay alive for this call.
unsafe fn owned(pointer: *const c_char, length: usize) -> String {
    if pointer.is_null() || length == 0 {
        return String::new();
    }
    // SAFETY: the caller guarantees `length` readable bytes at `pointer`.
    let bytes = unsafe { std::slice::from_raw_parts(pointer as *const u8, length) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Copies a NUL-terminated message LLVM allocated, then frees the original.
///
/// # Safety
/// `pointer` must be null or a message whose ownership an LLVM API handed
/// over, such as `LLVMPrintTypeToString`.
unsafe fn owned_message(pointer: *mut c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees a NUL-terminated message it owns.
    let text = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: ownership was transferred to this call, so it is freed once.
    unsafe { LLVMDisposeMessage(pointer) };
    text
}

/// The name of a value, which LLVM returns length-delimited.
///
/// # Safety
/// `value` must be a valid `LLVMValueRef` in a live context.
unsafe fn value_name(value: LLVMValueRef) -> String {
    let mut length = 0usize;
    // SAFETY: the caller guarantees a live value, and `LLVMGetValueName2`
    // writes the length of the buffer it returns.
    unsafe { owned(LLVMGetValueName2(value, &mut length), length) }
}

/// Debug location of a function, global or instruction, with its inlining
/// chain.
///
/// Two hazards this guards. `LLVMGetDebugLoc*` accept functions, globals and
/// instructions, but `LLVMInstructionGetDebugLoc` requires an instruction and
/// must be reached only after `LLVMIsAInstruction`. And every frame in an
/// inlining chain has its own file, so the leaf's filename must not be copied
/// onto the frames above it.
///
/// # Safety
/// `value` must be a valid `LLVMValueRef` in a live context, and `module_id`
/// must name the module it came from.
unsafe fn location_of(
    value: LLVMValueRef,
    module_id: &str,
    source_status: &HashMap<(String, PathBuf), SourceStatus>,
) -> Option<SourceLocation> {
    // SAFETY: the caller guarantees a live value; this accessor accepts
    // functions, globals and instructions alike and answers 0 without one.
    let line = unsafe { LLVMGetDebugLocLine(value) };
    if line == 0 {
        return None;
    }

    // These return length-delimited buffers, not NUL-terminated strings.
    let mut length = 0;
    // SAFETY: the caller guarantees a live value, and the accessor writes the
    // length of the buffer it returns.
    let file = unsafe { owned(LLVMGetDebugLocFilename(value, &mut length), length as usize) };
    let mut length = 0;
    // SAFETY: as above, for the directory half of the same location.
    let directory = unsafe {
        owned(
            LLVMGetDebugLocDirectory(value, &mut length),
            length as usize,
        )
    };

    // The inlinedAt chain exists only on instructions.
    let mut inlined_at = Vec::new();
    // SAFETY: the caller guarantees a live value; `LLVMIsAInstruction` is the
    // guard that makes the instruction-only accessor below legal.
    if !unsafe { LLVMIsAInstruction(value) }.is_null() {
        // SAFETY: reached only for an instruction, as this accessor requires.
        let mut metadata = unsafe { LLVMInstructionGetDebugLoc(value) };
        while !metadata.is_null() {
            // SAFETY: `metadata` is a live `DILocation` from this module.
            let outer = unsafe { LLVMDILocationGetInlinedAt(metadata) };
            if outer.is_null() {
                break;
            }
            // SAFETY: `outer` is the live `DILocation` of the calling frame.
            inlined_at.push(unsafe { location_of_metadata(outer, module_id, source_status) });
            metadata = outer;
        }
    }

    // SAFETY: the caller guarantees a live value, and the column accessor
    // accepts everything the line accessor above already answered for.
    Some(unsafe {
        build_location(
            file,
            directory,
            line,
            LLVMGetDebugLocColumn(value),
            module_id,
            source_status,
            inlined_at,
        )
    })
}

/// One inlining frame, resolved through its own scope so it carries its own
/// file and directory rather than inheriting the leaf's.
///
/// # Safety
/// `location` must be a valid `DILocation` metadata reference.
unsafe fn location_of_metadata(
    location: LLVMMetadataRef,
    module_id: &str,
    source_status: &HashMap<(String, PathBuf), SourceStatus>,
) -> SourceLocation {
    // SAFETY: the caller guarantees a live `DILocation`, whose scope always
    // resolves to a file.
    let scope = unsafe { LLVMDILocationGetScope(location) };
    // SAFETY: `scope` is the live scope of that location.
    let scope_file = unsafe { LLVMDIScopeGetFile(scope) };
    let mut length = 0;
    // SAFETY: `scope_file` is live and the accessor writes the buffer length.
    let file = unsafe {
        owned(
            LLVMDIFileGetFilename(scope_file, &mut length),
            length as usize,
        )
    };
    let mut length = 0;
    // SAFETY: as above, for the directory half of the same file.
    let directory = unsafe {
        owned(
            LLVMDIFileGetDirectory(scope_file, &mut length),
            length as usize,
        )
    };
    // SAFETY: the caller guarantees a live `DILocation` for both accessors.
    unsafe {
        build_location(
            file,
            directory,
            LLVMDILocationGetLine(location),
            LLVMDILocationGetColumn(location),
            module_id,
            source_status,
            Vec::new(),
        )
    }
}

/// Assembles a location and resolves its source status, which is keyed by
/// module as well as path: two modules may record different hashes for the
/// same file.
#[allow(clippy::too_many_arguments)]
unsafe fn build_location(
    file: String,
    directory: String,
    line: u32,
    column: u32,
    module_id: &str,
    source_status: &HashMap<(String, PathBuf), SourceStatus>,
    inlined_at: Vec<SourceLocation>,
) -> SourceLocation {
    let file = PathBuf::from(file);
    let full = if directory.is_empty() {
        file.clone()
    } else {
        PathBuf::from(&directory).join(&file)
    };
    SourceLocation {
        file,
        directory: (!directory.is_empty()).then(|| PathBuf::from(directory)),
        line,
        column,
        source_status: source_status
            .get(&(module_id.to_string(), full))
            .copied()
            .unwrap_or(SourceStatus::Unknown),
        inlined_at,
    }
}

/// Only the linkages the facts distinguish. Everything else is `Other`
/// rather than being forced into a neighbouring meaning.
fn linkage_of(linkage: LLVMLinkage) -> Linkage {
    match linkage {
        LLVMLinkage::LLVMExternalLinkage => Linkage::External,
        LLVMLinkage::LLVMInternalLinkage | LLVMLinkage::LLVMPrivateLinkage => Linkage::Internal,
        LLVMLinkage::LLVMWeakAnyLinkage
        | LLVMLinkage::LLVMWeakODRLinkage
        | LLVMLinkage::LLVMLinkOnceAnyLinkage
        | LLVMLinkage::LLVMLinkOnceODRLinkage
        | LLVMLinkage::LLVMExternalWeakLinkage => Linkage::Weak,
        _ => Linkage::Other,
    }
}

/// Classifies a call or invoke by what it actually calls in this module.
/// A name is never rebound: a direct call records the callee as referenced
/// here.
///
/// # Safety
/// `instruction` must be a live call or invoke instruction.
unsafe fn call_target(instruction: LLVMValueRef, module_id: &str) -> CallTarget {
    // SAFETY: the caller guarantees a call or invoke, which this accessor
    // requires.
    let called = unsafe { LLVMGetCalledValue(instruction) };
    if !called.is_null() {
        // SAFETY: `called` is a live value in this module.
        if !unsafe { LLVMIsAFunction(called) }.is_null() {
            // SAFETY: as above.
            let symbol = unsafe { value_name(called) };
            return if symbol.starts_with("llvm.") {
                CallTarget::Intrinsic { name: symbol }
            } else {
                CallTarget::Direct {
                    callee: FunctionId {
                        module_id: module_id.to_string(),
                        symbol,
                    },
                }
            };
        }
        // SAFETY: as above.
        if !unsafe { LLVMIsAInlineAsm(called) }.is_null() {
            return CallTarget::InlineAsm;
        }
    }
    CallTarget::Indirect {
        // SAFETY: the caller guarantees a call or invoke, and
        // `LLVMPrintTypeToString` hands over the string it returns.
        signature: unsafe {
            owned_message(LLVMPrintTypeToString(LLVMGetCalledFunctionType(
                instruction,
            )))
        },
        // `!callees` is read in a later step; an unbounded site stays `None`
        // rather than claiming a bound nothing established.
        llvm_target_bound: None,
    }
}

/// The function an instruction belongs to, or `None` when it has no parent.
///
/// # Safety
/// `instruction` must be a live instruction.
unsafe fn enclosing_function(instruction: LLVMValueRef, module_id: &str) -> Option<FunctionId> {
    // SAFETY: the caller guarantees an instruction, which this accessor
    // requires.
    let block = unsafe { LLVMGetInstructionParent(instruction) };
    if block.is_null() {
        return None;
    }
    // SAFETY: `block` is a live basic block.
    let function = unsafe { LLVMGetBasicBlockParent(block) };
    if function.is_null() {
        return None;
    }
    Some(FunctionId {
        module_id: module_id.to_string(),
        // SAFETY: `function` is the live function owning that block.
        symbol: unsafe { value_name(function) },
    })
}

/// Records every use of `function` that is not a call with it in callee
/// position: those are already call sites, and counting them twice would
/// report an ordinary call as a taken address.
///
/// # Safety
/// `function` must be a live function in a live module.
unsafe fn collect_uses(
    function: LLVMValueRef,
    id: &FunctionId,
    module_id: &str,
    source_status: &HashMap<(String, PathBuf), SourceStatus>,
    uses: &mut Vec<UseFact>,
) {
    // SAFETY: the caller guarantees a live function.
    let mut current = unsafe { LLVMGetFirstUse(function) };
    while !current.is_null() {
        // SAFETY: `current` is a live use of that function.
        let user = unsafe { LLVMGetUser(current) };
        // SAFETY: as above; advancing here keeps the loop total even when the
        // body skips this use.
        current = unsafe { LLVMGetNextUse(current) };
        if user.is_null() {
            continue;
        }

        // SAFETY: `user` is a live value; `LLVMIsAInstruction` is the guard
        // that makes the instruction-only accessors below legal.
        let is_instruction = !unsafe { LLVMIsAInstruction(user) }.is_null();
        // SAFETY: reached only for an instruction, as this accessor requires.
        let opcode = is_instruction.then(|| unsafe { LLVMGetInstructionOpcode(user) });

        let is_call = matches!(opcode, Some(LLVMOpcode::LLVMCall | LLVMOpcode::LLVMInvoke));
        // SAFETY: reached only for a call or invoke, as this accessor
        // requires.
        if is_call && unsafe { LLVMGetCalledValue(user) } == function {
            continue;
        }

        let kind = match opcode {
            Some(LLVMOpcode::LLVMStore) => UseKind::StoredToMemory,
            Some(LLVMOpcode::LLVMCall | LLVMOpcode::LLVMInvoke) => UseKind::PassedAsArgument,
            Some(LLVMOpcode::LLVMRet) => UseKind::ReturnedValue,
            Some(_) => UseKind::Other,
            // SAFETY: `user` is a live value, which this accessor accepts.
            None if !unsafe { LLVMIsAGlobalVariable(user) }.is_null() => UseKind::GlobalInitializer,
            None => UseKind::Other,
        };

        uses.push(UseFact {
            used: id.clone(),
            // SAFETY: both are reached only for an instruction.
            in_function: is_instruction
                .then(|| unsafe { enclosing_function(user, module_id) })
                .flatten(),
            location: is_instruction
                .then(|| unsafe { location_of(user, module_id, source_status) })
                .flatten(),
            kind,
        });
    }
}

/// Names both the producer and the reader. A module a newer LLVM wrote is a
/// version mismatch, not an unexplained parse failure, and the catalog
/// already recorded which compiler wrote it.
fn parse_error(module: &LoadedModule, diagnostics: &[String]) -> Error {
    let reader = llvm_version();
    let detail = if diagnostics.is_empty() {
        String::new()
    } else {
        format!(" ({})", diagnostics.join("; "))
    };
    match module.record.compiler.as_ref() {
        Some(compiler) => Error::InvalidArguments(format!(
            "module {} was produced by {} and cannot be read by LLVM {reader}{detail}",
            module.id, compiler.version
        )),
        None => Error::InvalidArguments(format!(
            "module {} cannot be read by LLVM {reader}{detail}",
            module.id
        )),
    }
}

pub fn extract(
    module: &LoadedModule,
    source_status: &HashMap<(String, PathBuf), SourceStatus>,
) -> Result<ModuleFacts, Error> {
    // SAFETY: the context, buffer and module are created here, used only
    // within this function, and disposed before returning.
    unsafe { extract_inner(module, source_status) }
}

/// # Safety
/// No preconditions: every handle this touches is created, used and disposed
/// inside it. It is `unsafe` only because it drives the C API directly.
unsafe fn extract_inner(
    module: &LoadedModule,
    source_status: &HashMap<(String, PathBuf), SourceStatus>,
) -> Result<ModuleFacts, Error> {
    // Declaration order is drop order reversed: the parsed module is disposed
    // first, then the buffer, then the context, and the diagnostic sink last
    // because the context can still reach it while it is being disposed.
    let sink = DiagnosticSink::new();
    // SAFETY: `LLVMContextCreate` allocates a fresh context; the guard
    // disposes it on every exit path, including the error returns below.
    let context = Context(unsafe { LLVMContextCreate() });
    // SAFETY: the context is live, `collect_diagnostic` is an `extern "C"`
    // function that cannot unwind into LLVM, and the sink it writes to
    // outlives the context.
    unsafe { LLVMContextSetDiagnosticHandler(context.0, Some(collect_diagnostic), sink.0.cast()) };

    let bytes = &module.bytes;
    // SAFETY: the range is `module.bytes`, borrowed from the caller, so it
    // outlives this function and therefore the buffer. The name is a
    // NUL-terminated literal, and bitcode needs no NUL terminator.
    let buffer = Buffer(unsafe {
        LLVMCreateMemoryBufferWithMemoryRange(
            bytes.as_ptr().cast::<c_char>(),
            bytes.len(),
            c"rllvm-module".as_ptr(),
            0,
        )
    });

    let mut parsed: LLVMModuleRef = std::ptr::null_mut();
    // SAFETY: context and buffer are live and `parsed` is a valid out
    // pointer. The parse reads the buffer without taking ownership of it.
    let failed = unsafe { LLVMParseBitcodeInContext2(context.0, buffer.0, &mut parsed) } != 0;
    if failed || parsed.is_null() {
        return Err(parse_error(module, &sink.take()));
    }
    let parsed = ParsedModule(parsed);

    let mut functions = Vec::new();
    let mut call_sites = Vec::new();
    let mut uses = Vec::new();

    // SAFETY: `parsed` is a module in the live context.
    let mut function = unsafe { LLVMGetFirstFunction(parsed.0) };
    while !function.is_null() {
        let id = FunctionId {
            module_id: module.id.clone(),
            // SAFETY: `function` is a live function in that module.
            symbol: unsafe { value_name(function) },
        };

        // Lines any instruction maps to, which is what `at` answers from.
        // This is the location's own file, paired the way `SourceLocation`
        // records it, not a reconstructed absolute path.
        let mut mapped_lines = BTreeSet::new();

        let mut block_index = 0u32;
        // SAFETY: `function` is live; a declaration simply has no blocks.
        let mut block = unsafe { LLVMGetFirstBasicBlock(function) };
        while !block.is_null() {
            let mut instruction_index = 0u32;
            // SAFETY: `block` is a live basic block of that function.
            let mut instruction = unsafe { LLVMGetFirstInstruction(block) };
            while !instruction.is_null() {
                // SAFETY: `instruction` is live and belongs to `block`.
                let location = unsafe { location_of(instruction, &module.id, source_status) };
                if let Some(location) = &location {
                    mapped_lines.insert((location.file.clone(), location.line));
                }

                // SAFETY: as above.
                let opcode = unsafe { LLVMGetInstructionOpcode(instruction) };
                if matches!(opcode, LLVMOpcode::LLVMCall | LLVMOpcode::LLVMInvoke) {
                    call_sites.push(CallSiteFact {
                        // Two calls on one line differ here, not in the
                        // location: identity is positional.
                        id: CallSiteId {
                            function: id.clone(),
                            block_index,
                            instruction_index,
                        },
                        location,
                        // SAFETY: reached only for a call or invoke.
                        target: unsafe { call_target(instruction, &module.id) },
                    });
                }

                // SAFETY: `instruction` is live in `block`.
                instruction = unsafe { LLVMGetNextInstruction(instruction) };
                instruction_index += 1;
            }
            // SAFETY: `block` is live in `function`.
            block = unsafe { LLVMGetNextBasicBlock(block) };
            block_index += 1;
        }

        functions.push(FunctionFact {
            id: id.clone(),
            // SAFETY: `function` is a live global value, which all four
            // accessors accept.
            is_definition: unsafe { LLVMIsDeclaration(function) } == 0,
            linkage: linkage_of(unsafe { LLVMGetLinkage(function) }),
            // The value type, not `LLVMTypeOf`: an opaque pointer prints as
            // `ptr` and would record no signature at all.
            signature: unsafe {
                owned_message(LLVMPrintTypeToString(LLVMGlobalGetValueType(function)))
            },
            location: unsafe { location_of(function, &module.id, source_status) },
            mapped_lines,
        });

        // SAFETY: `function` is live in the module being walked.
        unsafe { collect_uses(function, &id, &module.id, source_status, &mut uses) };

        // SAFETY: as above.
        function = unsafe { LLVMGetNextFunction(function) };
    }

    // Dispose before reading the sink, so nothing LLVM emits while tearing
    // the module down is lost from the report.
    drop(parsed);
    drop(buffer);
    drop(context);
    let diagnostics = sink.take();

    Ok(ModuleFacts {
        functions,
        call_sites,
        uses,
        diagnostics,
    })
}
