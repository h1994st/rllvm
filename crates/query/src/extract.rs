//! The llvm-sys walk. This is the only module in the crate containing
//! `unsafe`, and no LLVM handle leaves it: every public value is owned Rust
//! data.
//!
//! Changing what this module produces requires bumping `FACTS_FORMAT` in
//! `cache.rs`: `the_facts_format_names_what_extraction_produces` says how.

use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    ffi::{CStr, c_char, c_int, c_void},
    path::PathBuf,
};

use llvm_sys::{
    LLVMDiagnosticSeverity, LLVMLinkage, LLVMOpcode, LLVMTypeKind,
    bit_reader::LLVMParseBitcodeInContext2,
    core::*,
    debuginfo::{
        LLVMDIFileGetDirectory, LLVMDIFileGetFilename, LLVMDIFlagStaticMember,
        LLVMDIGlobalVariableExpressionGetVariable, LLVMDILocationGetColumn,
        LLVMDILocationGetInlinedAt, LLVMDILocationGetLine, LLVMDILocationGetScope,
        LLVMDIScopeGetFile, LLVMDITypeGetFlags, LLVMDITypeGetName, LLVMDITypeGetOffsetInBits,
        LLVMDITypeGetSizeInBits, LLVMGetDINodeTag, LLVMGetMetadataKind, LLVMGetSubprogram,
        LLVMInstructionGetDebugLoc, LLVMMetadataKind,
    },
    error::{LLVMDisposeErrorMessage, LLVMErrorRef, LLVMGetErrorMessage},
    prelude::*,
    target::{LLVMABISizeOfType, LLVMGetModuleDataLayout, LLVMOffsetOfElement, LLVMTargetDataRef},
    transforms::pass_builder::{
        LLVMCreatePassBuilderOptions, LLVMDisposePassBuilderOptions, LLVMRunPasses,
    },
};

use rllvm_core::error::Error;

use crate::{
    facts::*,
    load::{LoadedModule, SourceState},
};

unsafe extern "C" {
    /// The Itanium C++ ABI demangler, from the C++ runtime LLVM already
    /// links. `llvm-c` exposes no demangler of its own (checked against the
    /// LLVM 23 headers), and `llvm::itaniumDemangle` is C++ with no C entry
    /// point, so this is the only one reachable without a new dependency.
    ///
    /// With a null output buffer it allocates the result with `malloc`, so
    /// the caller frees it with `free` -- not `LLVMDisposeMessage`, which
    /// happens to call `free` today but documents no such contract.
    fn __cxa_demangle(
        name: *const c_char,
        output: *mut c_char,
        length: *mut usize,
        status: *mut c_int,
    ) -> *mut c_char;

    /// Paired with `__cxa_demangle` above. Declared rather than pulled in
    /// through a `libc` dependency, matching how this module already reaches
    /// the C API through `std::ffi` alone.
    fn free(pointer: *mut c_void);
}

/// The C++ reading of a mangled symbol, or `None` when the name is not
/// mangled or the demangler refuses it.
///
/// Identity stays with the mangled symbol; this is for display. A name that
/// merely looks mangled produces `None` rather than a garbled guess, so an
/// absent reading is never mistaken for a real one.
///
/// Rust's legacy scheme is Itanium-shaped and demangles through the same C++
/// path, hash suffix and all. Its `v0` scheme is not, and is read by
/// `rustc-demangle` below -- without which a Rust frame in a cross-language
/// answer stays mangled while the C++ frames beside it read cleanly.
pub fn demangle(symbol: &str) -> Option<String> {
    // `_R` is the `v0` marker. The C++ demangler refuses these, so they are
    // read here instead; `try_demangle` rejects a name that only looks the
    // part, keeping the "no garbled guess" rule below.
    if let Some(rust) = symbol
        .starts_with("_R")
        .then(|| rustc_demangle::try_demangle(symbol).ok())
        .flatten()
    {
        return Some(format!("{rust:#}"));
    }
    // `_Z` is the Itanium ABI's marker for a mangled name, so a C program
    // never reaches the FFI call at all.
    if !symbol.starts_with("_Z") {
        return None;
    }
    // An interior NUL cannot be part of an LLVM symbol, but `CString`
    // rejecting one is cheaper than reasoning about it.
    let input = std::ffi::CString::new(symbol).ok()?;
    let mut status: c_int = 0;
    // SAFETY: `input` is a live NUL-terminated string for the call. A null
    // output buffer and length ask the demangler to allocate, which it does
    // with `malloc`; `status` is a valid out pointer.
    let output = unsafe {
        __cxa_demangle(
            input.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut status,
        )
    };
    if output.is_null() {
        return None;
    }
    // SAFETY: non-null, and the demangler returns a NUL-terminated string.
    let text = unsafe { CStr::from_ptr(output) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: ownership of a `malloc`ed buffer passed to this call, freed
    // once. Done before the status check below so no exit path leaks it.
    unsafe { free(output.cast()) };
    // A non-zero status with a non-null buffer is not documented to happen,
    // but the buffer is not a reading of the symbol if it does.
    (status == 0).then_some(text)
}

/// Version of the LLVM this binary links, from the C API.
pub fn llvm_version() -> String {
    let (mut major, mut minor, mut patch) = (0, 0, 0);
    // SAFETY: LLVMGetVersion writes three unsigned values and nothing else.
    unsafe { llvm_sys::core::LLVMGetVersion(&mut major, &mut minor, &mut patch) };
    format!("{major}.{minor}.{patch}")
}

/// The module id neutral facts carry until [`ModuleFacts::bind_to_catalog`]
/// stamps the catalog's. Extraction must not depend on the catalog: the same
/// bytes get a different id in every catalog that names them.
const NEUTRAL_MODULE_ID: &str = "";

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ModuleFacts {
    pub functions: Vec<FunctionFact>,
    pub call_sites: Vec<CallSiteFact>,
    pub uses: Vec<UseFact>,
    /// Quoted from `!llvm.ident`, in order.
    pub producers: Vec<String>,
    /// Non-fatal problems observed while extracting. Surfaced in the
    /// analysis report, not swallowed into logging.
    pub diagnostics: Vec<String>,
}

impl ModuleFacts {
    /// Stamps `module_id` on every function id and resolves every location's
    /// source status, inlined frames included. Runs on every load, whether the
    /// neutral facts were just extracted or read from the cache, so both paths
    /// produce the same facts.
    ///
    /// Status is keyed by module as well as path: two modules may record
    /// different hashes for the same file.
    pub fn bind_to_catalog(
        &mut self,
        module_id: &str,
        source_status: &HashMap<(String, PathBuf), SourceState>,
    ) {
        let stamp = |id: &mut FunctionId| id.module_id = module_id.to_string();
        let resolve = |location: &mut Option<SourceLocation>| {
            if let Some(location) = location {
                resolve_status(location, module_id, source_status);
            }
        };
        for function in &mut self.functions {
            stamp(&mut function.id);
            if let Some(target) = &mut function.alias_of {
                stamp(target);
            }
            resolve(&mut function.location);
        }
        for site in &mut self.call_sites {
            stamp(&mut site.id.function);
            resolve(&mut site.location);
            match &mut site.target {
                CallTarget::Direct { callee } => stamp(callee),
                CallTarget::Indirect {
                    llvm_target_bound: Some(bound),
                    ..
                } => bound.iter_mut().for_each(stamp),
                CallTarget::Indirect { .. }
                | CallTarget::Intrinsic { .. }
                | CallTarget::InlineAsm => {}
            }
        }
        for use_fact in &mut self.uses {
            stamp(&mut use_fact.used);
            if let Some(function) = &mut use_fact.in_function {
                stamp(function);
            }
            resolve(&mut use_fact.location);
        }
    }
}

/// One location and its inlining frames. The path is the one `build_location`
/// used to key on: the directory joined with the file, or the file alone.
fn resolve_status(
    location: &mut SourceLocation,
    module_id: &str,
    source_status: &HashMap<(String, PathBuf), SourceState>,
) {
    let full = match &location.directory {
        Some(directory) => directory.join(&location.file),
        None => location.file.clone(),
    };
    let state = source_status.get(&(module_id.to_string(), full)).copied();
    location.source_status = state.map_or(SourceStatus::Unknown, |state| state.status);
    location.status_basis = state.and_then(|state| state.basis);
    for frame in &mut location.inlined_at {
        resolve_status(frame, module_id, source_status);
    }
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

/// Copies an LLVM error's message and consumes the error.
///
/// Not a reuse of [`owned_message`]: `LLVMGetErrorMessage` documents its own
/// deallocator, `LLVMDisposeErrorMessage`, and LLVM implements the two
/// message families with different allocators (`new[]`/`delete[]` for
/// errors, `strdup`/`free` for the `Print*ToString` family `owned_message`
/// serves). Freeing one family's message with the other's deallocator is
/// undefined behaviour, not just a documentation mismatch.
///
/// # Safety
/// `error` must be null or a live, not-yet-consumed `LLVMErrorRef`.
unsafe fn owned_error_message(error: LLVMErrorRef) -> String {
    if error.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees a live, unconsumed error. This call
    // consumes it and returns a message now owned by this function.
    let message = unsafe { LLVMGetErrorMessage(error) };
    if message.is_null() {
        return String::new();
    }
    // SAFETY: `message` is non-null, and `LLVMGetErrorMessage` guarantees a
    // NUL-terminated string.
    let text = unsafe { CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: ownership passed to this call from `LLVMGetErrorMessage`, and
    // this is the deallocator its documentation pairs it with.
    unsafe { LLVMDisposeErrorMessage(message) };
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
/// `value` must be a valid `LLVMValueRef` in a live context.
unsafe fn location_of(value: LLVMValueRef) -> Option<SourceLocation> {
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
            inlined_at.push(unsafe { location_of_metadata(outer) });
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
            inlined_at,
        )
    })
}

/// One inlining frame, resolved through its own scope so it carries its own
/// file and directory rather than inheriting the leaf's.
///
/// # Safety
/// `location` must be a valid `DILocation` metadata reference.
unsafe fn location_of_metadata(location: LLVMMetadataRef) -> SourceLocation {
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
            Vec::new(),
        )
    }
}

/// Assembles a location with no source status: whether the file is current
/// is a fact about the catalog and the disk now, which
/// [`ModuleFacts::bind_to_catalog`] decides, not about the bitcode.
fn build_location(
    file: String,
    directory: String,
    line: u32,
    column: u32,
    inlined_at: Vec<SourceLocation>,
) -> SourceLocation {
    SourceLocation {
        file: PathBuf::from(file),
        directory: (!directory.is_empty()).then(|| PathBuf::from(directory)),
        line,
        column,
        source_status: SourceStatus::Unknown,
        status_basis: None,
        inlined_at,
    }
}

/// Only the linkages the facts distinguish. Everything else is `Other`
/// rather than being forced into a neighbouring meaning -- `extern_weak`
/// included, which is a declaration and so says nothing about a definition.
fn linkage_of(linkage: LLVMLinkage) -> Linkage {
    match linkage {
        LLVMLinkage::LLVMExternalLinkage => Linkage::External,
        LLVMLinkage::LLVMInternalLinkage | LLVMLinkage::LLVMPrivateLinkage => Linkage::Internal,
        LLVMLinkage::LLVMWeakODRLinkage | LLVMLinkage::LLVMLinkOnceODRLinkage => Linkage::Odr,
        LLVMLinkage::LLVMWeakAnyLinkage | LLVMLinkage::LLVMLinkOnceAnyLinkage => Linkage::Weak,
        LLVMLinkage::LLVMAvailableExternallyLinkage => Linkage::AvailableExternally,
        _ => Linkage::Other,
    }
}

/// How rustc's `!llvm.ident` entries begin, on every toolchain checked
/// (1.85 through 1.100 nightly), with or without `-g`.
const RUSTC_PRODUCER_PREFIX: &str = "rustc version ";

/// The operand of a `DISubprogram` that holds its `DICompileUnit`. Checked
/// against the node's kind before use, so a layout change reads as no
/// answer rather than as another node's text.
const SUBPROGRAM_UNIT_OPERAND: usize = 5;

/// The keys a printed `DICompileUnit` names its language under, and the
/// value that means Rust: DWARF 5 codes and DWARF 6 language names.
const DWARF_LANGUAGE_KEYS: [(&str, &str); 2] = [
    ("language: DW_LANG_", "Rust"),
    ("sourceLanguageName: DW_LNAME_", "Rust"),
];

/// Rust when every entry is rustc's, another language when none is.
/// Unknown when they are mixed -- a module `llvm-link` merged from both
/// cannot speak for any one function -- or when there are none.
fn producer_language(producers: &[String]) -> Option<Language> {
    let rustc = producers
        .iter()
        .filter(|producer| producer.starts_with(RUSTC_PRODUCER_PREFIX))
        .count();
    if producers.is_empty() || (rustc != 0 && rustc != producers.len()) {
        None
    } else if rustc == 0 {
        Some(Language::Other)
    } else {
        Some(Language::Rust)
    }
}

/// The language a printed `DICompileUnit` names, or `None` when it names
/// none this reader recognizes.
fn dwarf_language(unit: &str) -> Option<Language> {
    DWARF_LANGUAGE_KEYS.iter().find_map(|(key, rust)| {
        let start = unit.find(key)? + key.len();
        let name = unit[start..].split([',', ')']).next()?.trim();
        if name.is_empty() {
            None
        } else if name == *rust {
            Some(Language::Rust)
        } else {
            Some(Language::Other)
        }
    })
}

/// Every operand of an MDNode value, null operands included.
///
/// # Safety
/// `node` must be a live MDNode wrapped as a value.
unsafe fn md_node_operands(node: LLVMValueRef) -> Vec<LLVMValueRef> {
    // SAFETY: the caller guarantees a live MDNode value.
    let count = unsafe { LLVMGetMDNodeNumOperands(node) } as usize;
    let mut operands = vec![std::ptr::null_mut(); count];
    // SAFETY: `operands` holds exactly the `count` slots just reported.
    unsafe { LLVMGetMDNodeOperands(node, operands.as_mut_ptr()) };
    operands
}

/// A metadata operand as metadata, or `None` when it is absent or a value.
///
/// # Safety
/// `operand` must be null or a live operand of a metadata node.
unsafe fn operand_metadata(operand: LLVMValueRef) -> Option<LLVMMetadataRef> {
    // SAFETY: the caller guarantees a live operand. Only one wrapping
    // metadata is converted; a constant would be wrapped anew instead.
    (!operand.is_null() && !unsafe { LLVMIsAMDNode(operand) }.is_null())
        .then(|| unsafe { LLVMValueAsMetadata(operand) })
}

/// An `MDString` operand's text.
///
/// # Safety
/// `operand` must be null or a live operand of a metadata node.
unsafe fn operand_string(operand: LLVMValueRef) -> Option<String> {
    if operand.is_null() {
        return None;
    }
    let mut length = 0;
    // SAFETY: a live operand; anything but a string answers null.
    let text = unsafe { LLVMGetMDString(operand, &mut length) };
    // SAFETY: `text` spans `length` bytes when non-null.
    (!text.is_null()).then(|| unsafe { owned(text, length as usize) })
}

/// A constant integer operand, zero-extended.
///
/// # Safety
/// `operand` must be null or a live value.
unsafe fn operand_integer(operand: LLVMValueRef) -> Option<u64> {
    // SAFETY: a live value; the integer accessor is reached only for one.
    (!operand.is_null() && !unsafe { LLVMIsAConstantInt(operand) }.is_null())
        .then(|| unsafe { LLVMConstIntGetZExtValue(operand) })
}

/// Every `!llvm.ident` entry, in order.
///
/// # Safety
/// `module` must be a live module.
unsafe fn read_producers(module: LLVMModuleRef) -> Vec<String> {
    let name = c"llvm.ident";
    // SAFETY: live module, NUL-terminated name.
    let count = unsafe { LLVMGetNamedMetadataNumOperands(module, name.as_ptr()) } as usize;
    let mut nodes = vec![std::ptr::null_mut(); count];
    // SAFETY: `nodes` holds exactly the `count` slots just reported.
    unsafe { LLVMGetNamedMetadataOperands(module, name.as_ptr(), nodes.as_mut_ptr()) };
    nodes
        .into_iter()
        .filter(|node| !node.is_null())
        // SAFETY: a live operand of the named metadata, which is an MDNode,
        // whose operands are live.
        .filter_map(|node| unsafe { operand_string(*md_node_operands(node).first()?) })
        .collect()
}

/// How many aliases deep `alias_target` follows before giving up. The
/// verifier rejects alias cycles, so a real module never comes close; the
/// bound only keeps a malformed one from looping.
const MAX_ALIAS_DEPTH: usize = 16;

/// The function an alias stands for, following aliases of aliases. `None`
/// for an alias of data or of an expression that is not a function.
///
/// # Safety
/// `alias` must be a live global alias.
unsafe fn alias_target(alias: LLVMValueRef) -> Option<LLVMValueRef> {
    let mut value = alias;
    for _ in 0..MAX_ALIAS_DEPTH {
        // SAFETY: `value` is the live alias or a live aliasee of it.
        if !unsafe { LLVMIsAFunction(value) }.is_null() {
            return Some(value);
        }
        // SAFETY: as above.
        if unsafe { LLVMIsAGlobalAlias(value) }.is_null() {
            return None;
        }
        // SAFETY: `value` was just checked to be a global alias.
        value = unsafe { LLVMAliasGetAliasee(value) };
        if value.is_null() {
            return None;
        }
    }
    None
}

/// A definition's language: debug info first, since it names the
/// function's own unit, where the producer speaks for the module as a whole.
///
/// # Safety
/// `function` must be a live function in `context`.
unsafe fn definition_language(
    context: LLVMContextRef,
    function: LLVMValueRef,
    units: &mut HashMap<LLVMMetadataRef, Option<Language>>,
    module_language: Option<Language>,
) -> Option<SourceLanguage> {
    // SAFETY: the caller's guarantee, passed through.
    unsafe { debug_info_language(context, function, units) }
        .map(|name| SourceLanguage {
            name,
            basis: LanguageBasis::DebugInfo,
        })
        .or(module_language.map(|name| SourceLanguage {
            name,
            basis: LanguageBasis::Producer,
        }))
}

/// The language of the compile unit `function`'s subprogram belongs to.
///
/// The C API has no getter for a unit's language, so the unit is printed
/// and the language read from its text. Units are few and shared by many
/// functions, so each is printed once.
///
/// # Safety
/// `function` must be a live function in `context`.
unsafe fn debug_info_language(
    context: LLVMContextRef,
    function: LLVMValueRef,
    units: &mut HashMap<LLVMMetadataRef, Option<Language>>,
) -> Option<Language> {
    // SAFETY: the caller guarantees a live function.
    let subprogram = unsafe { LLVMGetSubprogram(function) };
    if subprogram.is_null() {
        return None;
    }
    // SAFETY: `subprogram` is live metadata in `context`.
    let node = unsafe { LLVMMetadataAsValue(context, subprogram) };
    // SAFETY: a `DISubprogram` is an MDNode.
    let unit = *unsafe { md_node_operands(node) }.get(SUBPROGRAM_UNIT_OPERAND)?;
    if unit.is_null() {
        return None;
    }
    // SAFETY: `unit` is a live metadata operand wrapped as a value.
    let metadata = unsafe { LLVMValueAsMetadata(unit) };
    // SAFETY: as above.
    let kind = unsafe { LLVMGetMetadataKind(metadata) };
    if !matches!(kind, LLVMMetadataKind::LLVMDICompileUnitMetadataKind) {
        return None;
    }
    *units.entry(metadata).or_insert_with(|| {
        // SAFETY: `unit` is live; the printed string is owned by this call.
        dwarf_language(&unsafe { owned_message(LLVMPrintValueToString(unit)) })
    })
}

/// Reads CVP's `!callees` metadata off an indirect call, if CVP attached
/// any.
///
/// `!callees` is an upper bound: it states that a defined execution of the
/// call cannot target a function outside the set. It is neither a claim
/// that these targets are reachable nor that the call executes at all.
///
/// # Safety
/// `instruction` must be a live call or invoke instruction in the context
/// that `callees_kind` was interned in.
unsafe fn indirect_target_bound(
    instruction: LLVMValueRef,
    callees_kind: u32,
    module_id: &str,
) -> Option<Vec<FunctionId>> {
    // SAFETY: the caller guarantees a live instruction and a kind ID from
    // its own context; a call this metadata kind was never attached to
    // simply answers null.
    let node = unsafe { LLVMGetMetadata(instruction, callees_kind) };
    if node.is_null() {
        return None;
    }
    // SAFETY: `node` is the live MDNode value CVP attached to `instruction`.
    let operands = unsafe { md_node_operands(node) };
    Some(
        operands
            .into_iter()
            .filter(|operand| !operand.is_null())
            .map(|operand| FunctionId {
                module_id: module_id.to_string(),
                // SAFETY: `operand` is a live, non-null operand of that node.
                symbol: unsafe { value_name(operand) },
            })
            .collect(),
    )
}

/// Classifies a call or invoke by what it actually calls in this module.
/// A name is never rebound: a direct call records the callee as referenced
/// here.
///
/// # Safety
/// `instruction` must be a live call or invoke instruction, and
/// `callees_kind` must be the "callees" metadata kind ID interned in the
/// context that owns it, and `fields` must read the module that owns it.
unsafe fn call_target(
    instruction: LLVMValueRef,
    module_id: &str,
    callees_kind: u32,
    fields: &FieldReader,
) -> CallTarget {
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
        // A call through an alias calls the function the alias stands for,
        // under the alias's own name. It is not indirect: nothing about the
        // target is unknown.
        // SAFETY: as above; `alias_target` is reached only for an alias.
        if !unsafe { LLVMIsAGlobalAlias(called) }.is_null()
            && unsafe { alias_target(called) }.is_some()
        {
            return CallTarget::Direct {
                callee: FunctionId {
                    module_id: module_id.to_string(),
                    // SAFETY: as above.
                    symbol: unsafe { value_name(called) },
                },
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
        // SAFETY: the caller guarantees a live call or invoke and a matching
        // kind ID. A site CVP could not bound stays `None` rather than
        // claiming a bound nothing established.
        llvm_target_bound: unsafe { indirect_target_bound(instruction, callees_kind, module_id) },
        // SAFETY: `called` is null or a live value in the module `fields`
        // reads.
        via_field: unsafe { fields.loaded_field(called) },
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

/// The prefixes clang gives an IR record type, of which one is stripped.
const RECORD_PREFIXES: [&str; 3] = ["struct.", "class.", "union."];

/// Clang's IR name for a record with no name of its own. Not a name to join
/// on: every anonymous record in a module answers to it.
const ANONYMOUS_RECORD: &str = "anon";

/// The marker of an Itanium type-name string, which C++ TBAA type nodes are
/// named by, and what the demangler reads one as.
const TYPEINFO_NAME_MARKER: &str = "_ZTS";
const TYPEINFO_NAME_PREFIX: &str = "typeinfo name for ";

/// How deep a TBAA walk goes before giving up. Real type trees are a few
/// levels; the bound only keeps malformed metadata from looping.
const MAX_TBAA_DEPTH: usize = 64;

/// How many typedefs, qualifiers and array levels a debug-info type walk
/// steps through before giving up, for the same reason.
const MAX_DEBUG_TYPE_DEPTH: usize = 64;

/// DWARF tags the field readers distinguish (DWARF 5, section 7.5.4).
const DW_TAG_ARRAY_TYPE: u16 = 0x01;
const DW_TAG_MEMBER: u16 = 0x0d;
const DW_TAG_TYPEDEF: u16 = 0x16;
const DW_TAG_CONST_TYPE: u16 = 0x26;
const DW_TAG_VOLATILE_TYPE: u16 = 0x35;
/// `class`, `structure` and `union`: the tags a field can belong to.
const DW_RECORD_TAGS: [u16; 3] = [0x02, 0x13, 0x17];

/// Operand positions in debug-info nodes, read through the generic MDNode
/// accessors because the C API has no getter for them. Each is read only
/// after the node's kind is checked. The layout is LLVM 21 and later
/// (llvm-sys 231): every `DIType` starts with five operands -- file, scope,
/// name, size and offset -- and its own begin after them. `DIDerivedType`
/// and `DICompositeType` share the base-type position.
const DI_TYPE_OPERANDS: usize = 5;
const DI_SCOPE_OPERAND: usize = 1;
const DI_BASE_TYPE_OPERAND: usize = DI_TYPE_OPERANDS;
const DI_COMPOSITE_ELEMENTS_OPERAND: usize = DI_TYPE_OPERANDS + 1;
const DI_COMPOSITE_IDENTIFIER_OPERAND: usize = DI_TYPE_OPERANDS + 4;
const DI_SUBROUTINE_TYPES_OPERAND: usize = DI_TYPE_OPERANDS;
const DI_VARIABLE_TYPE_OPERAND: usize = 3;
const SUBPROGRAM_TYPE_OPERAND: usize = 4;

/// One record name for every IR spelling of it: `struct.ops`, `struct.ops.12`
/// (a name `llvm-link` deduplicated) and plain `ops` all read `ops`, and a
/// C++ TBAA name such as `_ZTSN1n1SE` reads as the IR's `n::S`. `None` for a
/// record with no name of its own, which must never be joined on.
fn normalize_record(raw: &str) -> Option<String> {
    let name = RECORD_PREFIXES
        .iter()
        .find_map(|prefix| raw.strip_prefix(prefix))
        .unwrap_or(raw);
    let name = match name.rsplit_once('.') {
        Some((stem, suffix))
            if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            stem
        }
        _ => name,
    };
    let name = match name.strip_prefix(TYPEINFO_NAME_MARKER) {
        Some(mangled) => {
            let demangled = demangle(&format!("_Z{mangled}"))?;
            match demangled.strip_prefix(TYPEINFO_NAME_PREFIX) {
                Some(stripped) => stripped.to_string(),
                None => demangled,
            }
        }
        None => name.to_string(),
    };
    (!name.is_empty() && name != ANONYMOUS_RECORD).then_some(name)
}

/// The normalized name of a named IR struct type, `None` for a literal one
/// or an anonymous record.
///
/// # Safety
/// `ty` must be a live type.
unsafe fn struct_record(ty: LLVMTypeRef) -> Option<String> {
    // SAFETY: the caller guarantees a live type; the struct accessors are
    // reached only for a struct.
    unsafe {
        if !matches!(LLVMGetTypeKind(ty), LLVMTypeKind::LLVMStructTypeKind)
            || LLVMIsLiteralStruct(ty) != 0
        {
            return None;
        }
        let name = LLVMGetStructName(ty);
        if name.is_null() {
            return None;
        }
        normalize_record(&CStr::from_ptr(name).to_string_lossy())
    }
}

/// Whether `value` is an aggregate constant: a struct, array or vector.
///
/// # Safety
/// `value` must be a live value.
unsafe fn is_aggregate(value: LLVMValueRef) -> bool {
    // SAFETY: the caller guarantees a live value, which these accept.
    unsafe {
        !LLVMIsAConstantStruct(value).is_null()
            || !LLVMIsAConstantArray(value).is_null()
            || !LLVMIsAConstantVector(value).is_null()
    }
}

/// The position of `operand` among `user`'s operands.
///
/// # Safety
/// `user` must be a live user and `operand` one of its uses.
unsafe fn operand_index(user: LLVMValueRef, operand: LLVMUseRef) -> Option<u32> {
    // SAFETY: the caller guarantees a live user; every index asked for is
    // below the operand count it reports.
    let count = unsafe { LLVMGetNumOperands(user) }.max(0) as u32;
    (0..count).find(|&index| unsafe { LLVMGetOperandUse(user, index) } == operand)
}

/// The steps an address takes from a function up to whatever holds it:
/// each wrapping constant, innermost first, with the use of the level below
/// it among its operands.
type WrapPath = Vec<(LLVMValueRef, LLVMUseRef)>;

/// Reads which record field a pointer addresses, from the evidence the IR
/// carries at each optimization level. Built once per module, from handles
/// that module owns, and dropped with it.
struct FieldReader {
    context: LLVMContextRef,
    layout: LLVMTargetDataRef,
    tbaa_kind: u32,
    dbg_kind: u32,
    /// Source member names by field, from debug info. `None` where two
    /// members share an offset (a union, a bitfield), so no name is claimed.
    member_names: HashMap<FieldRef, Option<String>>,
}

impl FieldReader {
    /// # Safety
    /// `context` and `module` must be live, the module in that context.
    unsafe fn new(context: LLVMContextRef, module: LLVMModuleRef) -> Self {
        // SAFETY: the caller guarantees a live context and module. The data
        // layout is the module's own, not a copy to dispose. "tbaa" is 4
        // bytes and "dbg" 3.
        let mut reader = unsafe {
            Self {
                context,
                layout: LLVMGetModuleDataLayout(module),
                tbaa_kind: LLVMGetMDKindIDInContext(context, c"tbaa".as_ptr(), 4),
                dbg_kind: LLVMGetMDKindIDInContext(context, c"dbg".as_ptr(), 3),
                member_names: HashMap::new(),
            }
        };
        // SAFETY: as above.
        reader.member_names = unsafe { reader.read_member_names(module) };
        reader
    }

    fn evidence(&self, field: FieldRef, basis: FieldBasis) -> FieldEvidence {
        let name = self.member_names.get(&field).cloned().flatten();
        FieldEvidence { field, basis, name }
    }

    /// The field a called pointer was loaded from. Only a `load` names one:
    /// a pointer variable, a phi or a select has no field to name.
    ///
    /// # Safety
    /// `called` must be null or a live value in this reader's module.
    unsafe fn loaded_field(&self, called: LLVMValueRef) -> Option<FieldEvidence> {
        // SAFETY: a live value; the operand is read only from a load.
        unsafe {
            if called.is_null() || LLVMIsALoadInst(called).is_null() {
                return None;
            }
            self.access_field(called, LLVMGetOperand(called, 0))
        }
    }

    /// The field a load or store at `pointer` accesses: the typed GEP that
    /// computed `pointer`, else the TBAA tag on `access`.
    ///
    /// # Safety
    /// `access` must be a live load or store in this reader's module, and
    /// `pointer` its pointer operand.
    unsafe fn access_field(
        &self,
        access: LLVMValueRef,
        pointer: LLVMValueRef,
    ) -> Option<FieldEvidence> {
        // SAFETY: the caller's guarantee, passed through.
        if let Some(field) = unsafe { self.gep_field(pointer) } {
            return Some(self.evidence(field, FieldBasis::StructGep));
        }
        // SAFETY: as above.
        let field = unsafe { self.tbaa_field(access) }?;
        Some(self.evidence(field, FieldBasis::Tbaa))
    }

    /// A `getelementptr` over a named struct with constant indices, read
    /// through the module's data layout. The first index steps over whole
    /// records and does not move the field.
    ///
    /// # Safety
    /// `pointer` must be a live value in this reader's module.
    unsafe fn gep_field(&self, pointer: LLVMValueRef) -> Option<FieldRef> {
        // SAFETY: a live value; the GEP accessors are reached only for a GEP
        // instruction or expression, and every operand index is below the
        // count it reports.
        unsafe {
            let is_gep = !LLVMIsAGetElementPtrInst(pointer).is_null()
                || (!LLVMIsAConstantExpr(pointer).is_null()
                    && matches!(LLVMGetConstOpcode(pointer), LLVMOpcode::LLVMGetElementPtr));
            if !is_gep {
                return None;
            }
            let source = LLVMGetGEPSourceElementType(pointer);
            struct_record(source)?;
            let indices = (2..LLVMGetNumOperands(pointer).max(0) as u32)
                .map(|index| {
                    let operand = LLVMGetOperand(pointer, index);
                    (!LLVMIsAConstantInt(operand).is_null())
                        .then(|| LLVMConstIntGetSExtValue(operand))
                })
                .collect::<Option<Vec<_>>>()?;
            self.walk_field(source, &indices, None)
        }
    }

    /// Steps `indices` into `ty` from offset 0. A named struct level
    /// restarts the field there; a literal struct or an array level only
    /// moves it. The answer is the innermost named record, else `record`,
    /// and the offset since it.
    ///
    /// # Safety
    /// `ty` must be a live sized type in this reader's module.
    unsafe fn walk_field(
        &self,
        mut ty: LLVMTypeRef,
        indices: &[i64],
        mut record: Option<String>,
    ) -> Option<FieldRef> {
        let mut offset = 0u64;
        for &index in indices {
            // SAFETY: `ty` is live and sized; each accessor is reached only
            // for the type kind it requires, with an in-range element index.
            unsafe {
                match LLVMGetTypeKind(ty) {
                    LLVMTypeKind::LLVMStructTypeKind => {
                        let element = u32::try_from(index)
                            .ok()
                            .filter(|&element| element < LLVMCountStructElementTypes(ty))?;
                        let element_offset = LLVMOffsetOfElement(self.layout, ty, element);
                        if LLVMIsLiteralStruct(ty) == 0 {
                            // An anonymous record restarts the field too: it
                            // has no name, so nothing below it can be named.
                            record = struct_record(ty);
                            offset = element_offset;
                        } else {
                            offset = offset.checked_add(element_offset)?;
                        }
                        ty = LLVMStructGetTypeAtIndex(ty, element);
                    }
                    LLVMTypeKind::LLVMArrayTypeKind => {
                        let element = LLVMGetElementType(ty);
                        let step = u64::try_from(index)
                            .ok()?
                            .checked_mul(LLVMABISizeOfType(self.layout, element))?;
                        offset = offset.checked_add(step)?;
                        ty = element;
                    }
                    _ => return None,
                }
            }
        }
        Some(FieldRef {
            record: record?,
            offset,
        })
    }

    /// A TBAA struct-path tag `{base, access, offset}`, walked down from the
    /// base type to the member the access lands on. The record is the last
    /// struct passed through; a scalar tag, whose base is the access type,
    /// names none.
    ///
    /// # Safety
    /// `access` must be a live load or store in this reader's module.
    unsafe fn tbaa_field(&self, access: LLVMValueRef) -> Option<FieldRef> {
        // SAFETY: a live instruction and a kind interned in its context; an
        // instruction without the tag answers null.
        let tag = unsafe { LLVMGetMetadata(access, self.tbaa_kind) };
        if tag.is_null() {
            return None;
        }
        // SAFETY: `tag` is the live MDNode attached to `access`, and each
        // operand is read through the guarded accessors above.
        let (base, access_type, offset) = unsafe {
            let operands = md_node_operands(tag);
            (
                operand_metadata(*operands.first()?)?,
                operand_metadata(*operands.get(1)?)?,
                operand_integer(*operands.get(2)?)?,
            )
        };
        // SAFETY: both are live nodes of this module's TBAA tree.
        let ancestors = unsafe { self.tbaa_ancestors(access_type) };
        if ancestors.contains(&base) {
            return None;
        }
        let (mut node, mut remaining) = (base, offset);
        for _ in 0..MAX_TBAA_DEPTH {
            // SAFETY: `node` is a live node of that tree.
            let (name, members) = unsafe { self.tbaa_type(node) }?;
            let &(member, member_offset) = members
                .iter()
                .filter(|(_, member_offset)| *member_offset <= remaining)
                .max_by_key(|(member, member_offset)| {
                    (*member_offset, ancestors.contains(member))
                })?;
            if ancestors.contains(&member) {
                return Some(FieldRef {
                    record: normalize_record(&name)?,
                    offset: remaining,
                });
            }
            node = member;
            remaining -= member_offset;
        }
        None
    }

    /// A TBAA type node's name and its `(member, offset)` pairs. A scalar
    /// node lists its parent as its one member at offset 0.
    ///
    /// # Safety
    /// `node` must be live metadata in this reader's context.
    unsafe fn tbaa_type(
        &self,
        node: LLVMMetadataRef,
    ) -> Option<(String, Vec<(LLVMMetadataRef, u64)>)> {
        // SAFETY: the caller guarantees live metadata; only an `MDTuple`, the
        // shape every TBAA node has, is read as one.
        unsafe {
            if !matches!(
                LLVMGetMetadataKind(node),
                LLVMMetadataKind::LLVMMDTupleMetadataKind
            ) {
                return None;
            }
            let operands = md_node_operands(LLVMMetadataAsValue(self.context, node));
            let name = operand_string(*operands.first()?)?;
            let members = operands[1..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[member, offset]| {
                    Some((operand_metadata(member)?, operand_integer(offset)?))
                })
                .collect::<Option<Vec<_>>>()?;
            Some((name, members))
        }
    }

    /// `access_type` and every type above it, up to the TBAA root.
    ///
    /// # Safety
    /// `access_type` must be live metadata in this reader's context.
    unsafe fn tbaa_ancestors(&self, access_type: LLVMMetadataRef) -> Vec<LLVMMetadataRef> {
        let mut ancestors = vec![access_type];
        for _ in 0..MAX_TBAA_DEPTH {
            let current = ancestors[ancestors.len() - 1];
            // SAFETY: `current` is a live node of the same tree.
            let Some((_, members)) = (unsafe { self.tbaa_type(current) }) else {
                break;
            };
            match members.first() {
                Some(&(parent, _)) if !ancestors.contains(&parent) => ancestors.push(parent),
                _ => break,
            }
        }
        ancestors
    }

    /// The field an address goes into when `store` stores it. Only the
    /// stored value counts, and only the address itself: an aggregate that
    /// merely contains it occupies a different field.
    ///
    /// # Safety
    /// `store` must be a live store in this reader's module, `operand` the
    /// use that reached it and `path` the wrapping constants below that.
    unsafe fn stored_field(
        &self,
        store: LLVMValueRef,
        operand: LLVMUseRef,
        path: &WrapPath,
    ) -> Option<FieldEvidence> {
        // SAFETY: a live store, whose two operands always exist; the path's
        // constants are live.
        unsafe {
            if LLVMGetOperandUse(store, 0) != operand
                || path.iter().any(|&(wrapper, _)| is_aggregate(wrapper))
            {
                return None;
            }
            self.access_field(store, LLVMGetOperand(store, 1))
        }
    }

    /// The field an address occupies in a global's initializer: the
    /// innermost named struct around it, else, for an initializer of literal
    /// type, the record the global's debug info names.
    ///
    /// # Safety
    /// `global` must be a live global variable in this reader's module and
    /// `path` the wrapping constants from the address up to its initializer.
    unsafe fn initializer_field(
        &self,
        global: LLVMValueRef,
        path: &WrapPath,
    ) -> Option<FieldEvidence> {
        // SAFETY: every constant on the path is live, and each use on it is
        // one of its wrapper's operands.
        unsafe {
            // An expression over the address, such as a `ptrtoint`, can sit
            // below the aggregates; one between them is not a slot.
            let lowest = path
                .iter()
                .position(|&(wrapper, _)| is_aggregate(wrapper))?;
            let aggregates = &path[lowest..];
            if !aggregates.iter().all(|&(wrapper, _)| is_aggregate(wrapper)) {
                return None;
            }
            let indices = aggregates
                .iter()
                .map(|&(wrapper, operand)| operand_index(wrapper, operand).map(i64::from))
                .collect::<Option<Vec<_>>>()?;
            // From an aggregate at `level` down to the address.
            let below = |level: usize| indices[..=level].iter().rev().copied().collect::<Vec<_>>();
            let is_struct = |wrapper| !LLVMIsAConstantStruct(wrapper).is_null();

            if let Some(level) = aggregates.iter().position(|&(wrapper, _)| {
                is_struct(wrapper) && struct_record(LLVMTypeOf(wrapper)).is_some()
            }) {
                let ty = LLVMTypeOf(aggregates[level].0);
                let field = self.walk_field(ty, &below(level), None)?;
                return Some(self.evidence(field, FieldBasis::Initializer));
            }

            let level = aggregates
                .iter()
                .position(|&(wrapper, _)| is_struct(wrapper))?;
            let above = &aggregates[level + 1..];
            if !above
                .iter()
                .all(|&(wrapper, _)| !LLVMIsAConstantArray(wrapper).is_null())
            {
                return None;
            }
            let ty = LLVMTypeOf(aggregates[level].0);
            let record = self.debug_info_record(global, above.len(), ty)?;
            let field = self.walk_field(ty, &below(level), Some(record))?;
            Some(self.evidence(field, FieldBasis::DebugInfo))
        }
    }

    /// The type of a global's `DIGlobalVariable`, when it has one.
    ///
    /// # Safety
    /// `global` must be a live global variable in this reader's module.
    unsafe fn global_debug_type(&self, global: LLVMValueRef) -> Option<LLVMMetadataRef> {
        // SAFETY: a live global; the entries are read within their count and
        // disposed once, and each node is read only after its kind is checked.
        unsafe {
            let mut count = 0;
            let entries = LLVMGlobalCopyAllMetadata(global, &mut count);
            if entries.is_null() {
                return None;
            }
            let expression = (0..count as u32)
                .find(|&index| LLVMValueMetadataEntriesGetKind(entries, index) == self.dbg_kind)
                .map(|index| LLVMValueMetadataEntriesGetMetadata(entries, index));
            LLVMDisposeValueMetadataEntries(entries);
            let expression = expression.filter(|&expression| {
                matches!(
                    LLVMGetMetadataKind(expression),
                    LLVMMetadataKind::LLVMDIGlobalVariableExpressionMetadataKind
                )
            })?;
            let variable = LLVMDIGlobalVariableExpressionGetVariable(expression);
            if variable.is_null()
                || !matches!(
                    LLVMGetMetadataKind(variable),
                    LLVMMetadataKind::LLVMDIGlobalVariableMetadataKind
                )
            {
                return None;
            }
            self.node_operand(variable, DI_VARIABLE_TYPE_OPERAND)
        }
    }

    /// The record a literal-typed initializer holds, from the global's
    /// debug-info type: through typedefs and qualifiers, and through one
    /// array for each of the `arrays` levels above the record in the IR.
    /// Refused when the debug-info record is not the IR record's size, since
    /// then the two do not describe the same bytes.
    ///
    /// # Safety
    /// `global` must be a live global variable in this reader's module and
    /// `literal` a live literal struct type.
    unsafe fn debug_info_record(
        &self,
        global: LLVMValueRef,
        mut arrays: usize,
        literal: LLVMTypeRef,
    ) -> Option<String> {
        // SAFETY: the caller's guarantees; each node is read only after its
        // kind is checked, and a DWARF tag only from a debug-info type.
        unsafe {
            let mut ty = self.global_debug_type(global)?;
            let mut typedef = None;
            for _ in 0..MAX_DEBUG_TYPE_DEPTH {
                match LLVMGetMetadataKind(ty) {
                    LLVMMetadataKind::LLVMDIDerivedTypeMetadataKind => match LLVMGetDINodeTag(ty) {
                        DW_TAG_TYPEDEF => typedef = debug_type_name(ty),
                        DW_TAG_CONST_TYPE | DW_TAG_VOLATILE_TYPE => {}
                        _ => return None,
                    },
                    LLVMMetadataKind::LLVMDICompositeTypeMetadataKind => match LLVMGetDINodeTag(ty)
                    {
                        DW_TAG_ARRAY_TYPE => {
                            // One debug-info array can stand for several IR
                            // levels: `a[2][3]` has one node, two subranges.
                            let dimensions = self
                                .node_operand(ty, DI_COMPOSITE_ELEMENTS_OPERAND)
                                .map_or(0, |elements| self.node_elements(elements).len())
                                .max(1);
                            arrays = arrays.checked_sub(dimensions)?;
                            typedef = None;
                        }
                        tag if DW_RECORD_TAGS.contains(&tag) => {
                            let size = LLVMDITypeGetSizeInBits(ty);
                            if arrays != 0 || size != LLVMABISizeOfType(self.layout, literal) * 8 {
                                return None;
                            }
                            return self.debug_record_name(ty, typedef);
                        }
                        _ => return None,
                    },
                    _ => return None,
                }
                ty = self.node_operand(ty, DI_BASE_TYPE_OPERAND)?;
            }
            None
        }
    }

    /// A debug-info record's name: its C++ identifier when it has one, else,
    /// for a record at file scope, its own name or the typedef it was
    /// reached through.
    ///
    /// A record without an identifier inside a namespace or another record
    /// gets none. Its debug-info name is unqualified (`S`), while IR and
    /// TBAA qualify it (`(anonymous namespace)::S`), so the plain name would
    /// join it with an unrelated `::S`. Only C, whose records all sit at file
    /// scope, is named by its plain name.
    ///
    /// # Safety
    /// `record` must be a live `DICompositeType` in this reader's context.
    unsafe fn debug_record_name(
        &self,
        record: LLVMMetadataRef,
        typedef: Option<String>,
    ) -> Option<String> {
        // SAFETY: the caller guarantees a live composite type; its scope is
        // live metadata, read only for its kind.
        unsafe {
            let operands = md_node_operands(LLVMMetadataAsValue(self.context, record));
            if let Some(identifier) = operands
                .get(DI_COMPOSITE_IDENTIFIER_OPERAND)
                .and_then(|&identifier| operand_string(identifier))
            {
                return normalize_record(&identifier);
            }
            let at_file_scope = self
                .node_operand(record, DI_SCOPE_OPERAND)
                .is_none_or(|scope| {
                    matches!(
                        LLVMGetMetadataKind(scope),
                        LLVMMetadataKind::LLVMDIFileMetadataKind
                            | LLVMMetadataKind::LLVMDICompileUnitMetadataKind
                    )
                });
            if !at_file_scope {
                return None;
            }
            debug_type_name(record)
                .and_then(|name| normalize_record(&name))
                .or(typedef)
        }
    }

    /// The node at `index` among `node`'s operands.
    ///
    /// # Safety
    /// `node` must be a live MDNode in this reader's context.
    unsafe fn node_operand(&self, node: LLVMMetadataRef, index: usize) -> Option<LLVMMetadataRef> {
        // SAFETY: the caller guarantees a live node.
        unsafe {
            let operands = md_node_operands(LLVMMetadataAsValue(self.context, node));
            operand_metadata(*operands.get(index)?)
        }
    }

    /// The nodes of a tuple, such as a record's members.
    ///
    /// # Safety
    /// `tuple` must be live metadata in this reader's context.
    unsafe fn node_elements(&self, tuple: LLVMMetadataRef) -> Vec<LLVMMetadataRef> {
        // SAFETY: the caller guarantees live metadata; only a tuple is read.
        unsafe {
            if !matches!(
                LLVMGetMetadataKind(tuple),
                LLVMMetadataKind::LLVMMDTupleMetadataKind
            ) {
                return Vec::new();
            }
            md_node_operands(LLVMMetadataAsValue(self.context, tuple))
                .into_iter()
                .filter_map(|operand| operand_metadata(operand))
                .collect()
        }
    }

    /// Member names for every record debug info describes and a defined
    /// function's parameters or a global reach, keyed the way fields are.
    /// Empty without debug info.
    ///
    /// # Safety
    /// `module` must be this reader's live module.
    unsafe fn read_member_names(&self, module: LLVMModuleRef) -> HashMap<FieldRef, Option<String>> {
        let mut pending: Vec<(LLVMMetadataRef, Option<String>)> = Vec::new();
        // SAFETY: a live module; every global and function is live in it,
        // and each node is read only after its kind is checked.
        unsafe {
            let mut function = LLVMGetFirstFunction(module);
            while !function.is_null() {
                let subprogram = LLVMGetSubprogram(function);
                if LLVMIsDeclaration(function) == 0
                    && !subprogram.is_null()
                    && matches!(
                        LLVMGetMetadataKind(subprogram),
                        LLVMMetadataKind::LLVMDISubprogramMetadataKind
                    )
                    && let Some(ty) = self.node_operand(subprogram, SUBPROGRAM_TYPE_OPERAND)
                {
                    pending.push((ty, None));
                }
                function = LLVMGetNextFunction(function);
            }
            let mut global = LLVMGetFirstGlobal(module);
            while !global.is_null() {
                if let Some(ty) = self.global_debug_type(global) {
                    pending.push((ty, None));
                }
                global = LLVMGetNextGlobal(global);
            }
        }

        let mut names: HashMap<FieldRef, Option<String>> = HashMap::new();
        let mut visited = HashSet::new();
        while let Some((ty, typedef)) = pending.pop() {
            if !visited.insert(ty) {
                continue;
            }
            // SAFETY: `ty` is live metadata reached from the module's debug
            // info; each accessor is reached only for the kind it requires.
            unsafe {
                let base = || self.node_operand(ty, DI_BASE_TYPE_OPERAND);
                match LLVMGetMetadataKind(ty) {
                    LLVMMetadataKind::LLVMDISubroutineTypeMetadataKind => {
                        if let Some(types) = self.node_operand(ty, DI_SUBROUTINE_TYPES_OPERAND) {
                            pending
                                .extend(self.node_elements(types).into_iter().map(|ty| (ty, None)));
                        }
                    }
                    LLVMMetadataKind::LLVMDIDerivedTypeMetadataKind => {
                        let typedef = (LLVMGetDINodeTag(ty) == DW_TAG_TYPEDEF)
                            .then(|| debug_type_name(ty))
                            .flatten();
                        pending.extend(base().map(|base| (base, typedef)));
                    }
                    LLVMMetadataKind::LLVMDICompositeTypeMetadataKind => {
                        let tag = LLVMGetDINodeTag(ty);
                        if !DW_RECORD_TAGS.contains(&tag) {
                            pending.extend(base().map(|base| (base, None)));
                            continue;
                        }
                        let record = self.debug_record_name(ty, typedef);
                        let members = self
                            .node_operand(ty, DI_COMPOSITE_ELEMENTS_OPERAND)
                            .map(|elements| self.node_elements(elements))
                            .unwrap_or_default();
                        for member in members {
                            if !matches!(
                                LLVMGetMetadataKind(member),
                                LLVMMetadataKind::LLVMDIDerivedTypeMetadataKind
                            ) {
                                continue;
                            }
                            pending.extend(
                                self.node_operand(member, DI_BASE_TYPE_OPERAND)
                                    .map(|base| (base, None)),
                            );
                            let is_field = LLVMGetDINodeTag(member) == DW_TAG_MEMBER
                                && LLVMDITypeGetFlags(member) & LLVMDIFlagStaticMember == 0;
                            if let (true, Some(record), Some(name)) =
                                (is_field, &record, debug_type_name(member))
                            {
                                let field = FieldRef {
                                    record: record.clone(),
                                    offset: LLVMDITypeGetOffsetInBits(member) / 8,
                                };
                                names
                                    .entry(field)
                                    .and_modify(|known| {
                                        if known.as_deref() != Some(name.as_str()) {
                                            *known = None;
                                        }
                                    })
                                    .or_insert(Some(name));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        names
    }
}

/// A debug-info type's own name, `None` when it has none.
///
/// # Safety
/// `ty` must be a live debug-info type.
unsafe fn debug_type_name(ty: LLVMMetadataRef) -> Option<String> {
    let mut length = 0;
    // SAFETY: the caller guarantees a live type; the name spans `length`
    // bytes.
    let name = unsafe { owned(LLVMDITypeGetName(ty, &mut length), length) };
    (!name.is_empty()).then_some(name)
}

/// The constants an address can sit inside on its way to the value that
/// holds it: an expression over it, or an aggregate such as a table entry.
const WRAPPING_CONSTANTS: [unsafe extern "C" fn(LLVMValueRef) -> LLVMValueRef; 4] = [
    LLVMIsAConstantExpr,
    LLVMIsAConstantStruct,
    LLVMIsAConstantArray,
    LLVMIsAConstantVector,
];

/// One use of an address on the walk out from a function: what is used,
/// the user, the use itself, and the wrapping constants passed through.
struct Occurrence {
    used: LLVMValueRef,
    user: LLVMValueRef,
    operand: LLVMUseRef,
    path: WrapPath,
}

/// Each use of `value`, in use-list order, reached through `path`.
///
/// # Safety
/// `value` must be a live value in a live module.
unsafe fn users_of(value: LLVMValueRef, path: &WrapPath) -> Vec<Occurrence> {
    let mut users = Vec::new();
    // SAFETY: the caller guarantees a live value.
    let mut current = unsafe { LLVMGetFirstUse(value) };
    while !current.is_null() {
        // SAFETY: `current` is a live use of that value.
        let user = unsafe { LLVMGetUser(current) };
        if !user.is_null() {
            users.push(Occurrence {
                used: value,
                user,
                operand: current,
                path: path.clone(),
            });
        }
        // SAFETY: as above.
        current = unsafe { LLVMGetNextUse(current) };
    }
    users
}

/// Records every use of `function` that is not a call with it in callee
/// position: those are already call sites, and counting them twice would
/// report an ordinary call as a taken address.
///
/// A use inside a wrapping constant is followed to whatever uses that
/// constant, so an address in a dispatch table is attributed to the global
/// holding the table rather than to an anonymous constant. The constants
/// passed through are kept, so the slot the address occupies can be named.
///
/// # Safety
/// `function` must be a live function in the module `fields` reads.
unsafe fn collect_uses(
    function: LLVMValueRef,
    id: &FunctionId,
    module_id: &str,
    fields: &FieldReader,
    uses: &mut Vec<UseFact>,
) {
    // SAFETY: the caller guarantees a live function.
    let mut pending: VecDeque<_> = unsafe { users_of(function, &Vec::new()) }.into();
    while let Some(occurrence) = pending.pop_front() {
        let Occurrence {
            used,
            user,
            operand,
            path,
        } = occurrence;
        // SAFETY: `user` is a live value; `LLVMIsAInstruction` is the guard
        // that makes the instruction-only accessors below legal.
        let is_instruction = !unsafe { LLVMIsAInstruction(user) }.is_null();
        // SAFETY: reached only for an instruction, as this accessor requires.
        let opcode = is_instruction.then(|| unsafe { LLVMGetInstructionOpcode(user) });

        let is_call = matches!(opcode, Some(LLVMOpcode::LLVMCall | LLVMOpcode::LLVMInvoke));
        // SAFETY: reached only for a call or invoke, as this accessor
        // requires.
        if is_call && unsafe { LLVMGetCalledValue(user) } == used {
            continue;
        }

        // SAFETY: `user` is a live value, which these accessors accept.
        let is_global = !unsafe { LLVMIsAGlobalVariable(user) }.is_null();
        let wraps = !is_instruction
            && !is_global
            && WRAPPING_CONSTANTS
                .iter()
                .any(|is_a| !unsafe { is_a(user) }.is_null());
        if wraps {
            let mut path = path;
            path.push((user, operand));
            // SAFETY: `user` is a live constant.
            pending.extend(unsafe { users_of(user, &path) });
            continue;
        }

        let kind = match opcode {
            Some(LLVMOpcode::LLVMStore) => UseKind::StoredToMemory,
            Some(LLVMOpcode::LLVMCall | LLVMOpcode::LLVMInvoke) => UseKind::PassedAsArgument,
            Some(LLVMOpcode::LLVMRet) => UseKind::ReturnedValue,
            Some(_) => UseKind::Other,
            None if is_global => UseKind::GlobalInitializer,
            None => UseKind::Other,
        };
        // SAFETY: a store is a live instruction and a global a live global
        // variable, each reached through `operand` along `path`.
        let field = match kind {
            UseKind::StoredToMemory => unsafe { fields.stored_field(user, operand, &path) },
            UseKind::GlobalInitializer => unsafe { fields.initializer_field(user, &path) },
            _ => None,
        };

        uses.push(UseFact {
            used: id.clone(),
            // SAFETY: both are reached only for an instruction.
            in_function: is_instruction
                .then(|| unsafe { enclosing_function(user, module_id) })
                .flatten(),
            // SAFETY: reached only for a live global variable.
            in_global: is_global.then(|| unsafe { value_name(user) }),
            location: is_instruction
                .then(|| unsafe { location_of(user) })
                .flatten(),
            kind,
            field,
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

/// Facts that depend on the bytes alone: every module id is empty and every
/// location's status `unknown`. What is cached (see `cache.rs`).
/// `module` supplies the bytes and, for a parse error, its id and compiler.
pub fn extract_neutral(module: &LoadedModule) -> Result<ModuleFacts, Error> {
    // SAFETY: the context, buffer and module are created here, used only
    // within this function, and disposed before returning.
    unsafe { extract_inner(module) }
}

/// Neutral extraction bound to `module`'s catalog id and the source status
/// observed at load.
pub fn extract(
    module: &LoadedModule,
    source_status: &HashMap<(String, PathBuf), SourceState>,
) -> Result<ModuleFacts, Error> {
    let mut facts = extract_neutral(module)?;
    facts.bind_to_catalog(&module.id, source_status);
    Ok(facts)
}

/// # Safety
/// No preconditions: every handle this touches is created, used and disposed
/// inside it. It is `unsafe` only because it drives the C API directly.
unsafe fn extract_inner(module: &LoadedModule) -> Result<ModuleFacts, Error> {
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

    let mut pass_diagnostics = Vec::new();
    // CVP only attaches metadata; it rewrites no instructions, and the
    // bitcode on disk is never touched. Propagation is per module: a
    // callback assigned in another translation unit stays unresolved.
    // SAFETY: `parsed.0` is the module just parsed, live in `context.0`.
    // The target machine is null because `called-value-propagation` is a
    // module pass that needs none, which `LLVMRunPasses` accepts. `options`
    // is created and disposed within this block, never used afterward.
    unsafe {
        let options = LLVMCreatePassBuilderOptions();
        let error = LLVMRunPasses(
            parsed.0,
            c"called-value-propagation".as_ptr(),
            std::ptr::null_mut(),
            options,
        );
        LLVMDisposePassBuilderOptions(options);
        if !error.is_null() {
            // Not fatal -- bounds are opportunistic -- but it changes what
            // the answers can contain, so it belongs in the report rather
            // than in a debug log.
            let message = owned_error_message(error);
            pass_diagnostics.push(format!("called-value-propagation did not run: {message}"));
        }
    }

    // SAFETY: `context.0` is live; "callees" is 7 bytes, the metadata kind
    // CVP attaches its upper bound under.
    let callees_kind = unsafe { LLVMGetMDKindIDInContext(context.0, c"callees".as_ptr(), 7) };
    // SAFETY: `parsed` is a module in the live context, and the reader is
    // not used after the module is dropped below.
    let fields = unsafe { FieldReader::new(context.0, parsed.0) };

    // SAFETY: `parsed` is a module in the live context.
    let producers = unsafe { read_producers(parsed.0) };
    let module_language = producer_language(&producers);
    let mut units = HashMap::new();

    let mut functions = Vec::new();
    let mut call_sites = Vec::new();
    let mut uses = Vec::new();

    // SAFETY: `parsed` is a module in the live context.
    let mut function = unsafe { LLVMGetFirstFunction(parsed.0) };
    while !function.is_null() {
        let id = FunctionId {
            module_id: NEUTRAL_MODULE_ID.to_string(),
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
                let location = unsafe { location_of(instruction) };
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
                        target: unsafe {
                            call_target(instruction, NEUTRAL_MODULE_ID, callees_kind, &fields)
                        },
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

        // SAFETY: `function` is a live global value.
        let is_definition = unsafe { LLVMIsDeclaration(function) } == 0;

        functions.push(FunctionFact {
            id: id.clone(),
            is_definition,
            // SAFETY: `function` is a live global value, which these
            // accessors accept.
            linkage: linkage_of(unsafe { LLVMGetLinkage(function) }),
            // A declaration is not written in the module that declares it,
            // so it gets no language even when the module's producer or a
            // borrowed unit would otherwise suggest one.
            // SAFETY: `function` is live in `context`.
            language: if is_definition {
                unsafe { definition_language(context.0, function, &mut units, module_language) }
            } else {
                None
            },
            // The value type, not `LLVMTypeOf`: an opaque pointer prints as
            // `ptr` and would record no signature at all.
            signature: unsafe {
                owned_message(LLVMPrintTypeToString(LLVMGlobalGetValueType(function)))
            },
            location: unsafe { location_of(function) },
            mapped_lines,
            alias_of: None,
        });

        // SAFETY: `function` is live in the module being walked.
        unsafe { collect_uses(function, &id, NEUTRAL_MODULE_ID, &fields, &mut uses) };

        // SAFETY: as above.
        function = unsafe { LLVMGetNextFunction(function) };
    }

    // An alias is a definition under its own name with its target's body:
    // `rustc` emits one for an `extern "C"` wrapper that compiles to the same
    // code as the method it calls. It has no instructions to walk, so its
    // language and location are its target's, and queries follow
    // `alias_of` to the target's calls. An alias of data is not a function.
    // SAFETY: `parsed` is a module in the live context.
    let mut alias = unsafe { LLVMGetFirstGlobalAlias(parsed.0) };
    while !alias.is_null() {
        // SAFETY: `alias` is a live global alias in that module.
        if let Some(target) = unsafe { alias_target(alias) } {
            functions.push(FunctionFact {
                id: FunctionId {
                    module_id: NEUTRAL_MODULE_ID.to_string(),
                    // SAFETY: as above.
                    symbol: unsafe { value_name(alias) },
                },
                is_definition: true,
                // SAFETY: an alias is a live global value, which this accepts.
                linkage: linkage_of(unsafe { LLVMGetLinkage(alias) }),
                // SAFETY: `target` is a live function in `context`.
                language: unsafe {
                    definition_language(context.0, target, &mut units, module_language)
                },
                // SAFETY: as above, for the alias's own value type.
                signature: unsafe {
                    owned_message(LLVMPrintTypeToString(LLVMGlobalGetValueType(alias)))
                },
                // SAFETY: `target` is a live function.
                location: unsafe { location_of(target) },
                mapped_lines: BTreeSet::new(),
                alias_of: Some(FunctionId {
                    module_id: NEUTRAL_MODULE_ID.to_string(),
                    // SAFETY: `target` is a live function.
                    symbol: unsafe { value_name(target) },
                }),
            });
        }
        // SAFETY: `alias` is live in the module being walked.
        alias = unsafe { LLVMGetNextGlobalAlias(alias) };
    }

    // Dispose before reading the sink, so nothing LLVM emits while tearing
    // the module down is lost from the report.
    drop(parsed);
    drop(buffer);
    drop(context);
    let mut diagnostics = pass_diagnostics;
    diagnostics.extend(sink.take());

    Ok(ModuleFacts {
        functions,
        call_sites,
        uses,
        producers,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producers_decide_a_module_only_when_they_agree() {
        let rustc = "rustc version 1.98.0".to_string();
        let clang = "clang version 23.1.1".to_string();
        assert_eq!(
            producer_language(std::slice::from_ref(&rustc)),
            Some(Language::Rust)
        );
        assert_eq!(
            producer_language(std::slice::from_ref(&clang)),
            Some(Language::Other)
        );
        assert_eq!(producer_language(&[rustc, clang]), None, "merged");
        assert_eq!(producer_language(&[]), None, "no producer named");
    }

    #[test]
    fn a_printed_unit_names_its_language_under_either_key() {
        let unit = |text: &str| format!("distinct !DICompileUnit({text}, file: !1)");
        assert_eq!(
            dwarf_language(&unit("language: DW_LANG_Rust")),
            Some(Language::Rust)
        );
        assert_eq!(
            dwarf_language(&unit("language: DW_LANG_C11")),
            Some(Language::Other)
        );
        assert_eq!(
            dwarf_language(&unit("sourceLanguageName: DW_LNAME_Rust")),
            Some(Language::Rust)
        );
        assert_eq!(dwarf_language("distinct !DICompileUnit(file: !1)"), None);
    }

    /// The rows that matter are the refusals: a demangler that guessed at a
    /// name it does not understand would put a fabricated C++ signature in an
    /// answer, which is worse than printing the mangled name.
    #[test]
    fn demangling_reads_cxx_names_and_refuses_everything_else() {
        assert_eq!(
            demangle("_Z5twiceIiET_S0_").as_deref(),
            Some("int twice<int>(int)"),
            "the template instantiation from the ODR repro in #184"
        );
        assert_eq!(demangle("_ZN3FooC1Ev").as_deref(), Some("Foo::Foo()"));

        assert_eq!(demangle("main"), None, "a C name is not mangled");
        assert_eq!(demangle(""), None);
        assert_eq!(
            demangle("_Znotreallymangled"),
            None,
            "a name that only looks mangled must not produce a guess"
        );
        assert_eq!(
            demangle("_RNvC6foo3bar"),
            None,
            "malformed v0: a crate root carries a disambiguator"
        );
    }

    /// Rust `v0` symbols read through `rustc-demangle`, not the C++ path.
    ///
    /// A cross-language answer puts Rust and C++ frames in one list, and the
    /// C++ ones have always demangled. Leaving the Rust frames mangled made
    /// the boundary look like a defect in the answer rather than a property
    /// of the program -- this is the symbol quiche calls BoringSSL from.
    #[test]
    fn demangling_reads_rust_v0_names() {
        assert_eq!(
            demangle("_RNvMs2_NtCs8f0ESrtUyjS_6quiche3tlsNtB5_9Handshake12do_handshake").as_deref(),
            Some("<quiche::tls::Handshake>::do_handshake"),
        );

        // The legacy scheme keeps going through the C++ demangler, hash and
        // all, so that path is unchanged.
        assert!(
            demangle("_ZN4core3fmt5Debug3fmt17h0123456789abcdefE")
                .is_some_and(|name| name.contains("core::fmt")),
        );
    }

    /// A record has one name however the IR spells it, or field evidence
    /// from a typed GEP never meets the same field read through TBAA.
    #[test]
    fn record_names_normalize_across_ir_spellings() {
        for (raw, expected) in [
            ("struct.ops", Some("ops")),
            ("struct.ops.12", Some("ops")),
            ("struct.n::S", Some("n::S")),
            ("_ZTSN1n1SE", Some("n::S")),
            ("_ZTS6ns_ops", Some("ns_ops")),
            ("ops", Some("ops")),
            ("", None),
            ("struct.anon", None),
            ("union.anon.1", None),
        ] {
            assert_eq!(normalize_record(raw).as_deref(), expected, "{raw:?}");
        }
    }

    /// Rust's legacy scheme is Itanium-shaped, so it reads back. Pinned
    /// because rllvm captures Rust bitcode too, and this is what those
    /// answers will show.
    #[test]
    fn a_legacy_rust_symbol_reads_back_with_its_hash() {
        assert_eq!(
            demangle("_ZN4core3fmt5write17h1234567890abcdefE").as_deref(),
            Some("core::fmt::write::h1234567890abcdef")
        );
    }
}
