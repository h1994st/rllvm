//! Owned instruction-level IR of one module, which `extract.rs` fills from
//! bitcode. No LLVM handles: the guards analysis walks this in safe Rust.
//!
//! Function ids carry no module id, like neutral facts: the IR is read from
//! bytes alone, so it cannot know which catalog module they came from.

use crate::facts::{FunctionId, GuardKind, SourceLocation};

/// Dense index into [`FunctionIr::values`]. Parameters get ids too, ahead of
/// every instruction.
pub(crate) type ValueId = u32;

/// Every function the module defines, in module order. Declarations have no
/// body to read and are left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleIr {
    pub(crate) functions: Vec<FunctionIr>,
    /// What LLVM reported while reading the module, such as debug info it
    /// stripped. Empty when it said nothing.
    pub(crate) diagnostics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FunctionIr {
    pub(crate) id: FunctionId,
    /// One per formal parameter, in order.
    pub(crate) params: Vec<ValueId>,
    /// In layout order; the first is the entry block.
    pub(crate) blocks: Vec<BlockIr>,
    /// The parameters, then every instruction in block order.
    pub(crate) values: Vec<ValueNode>,
    /// Every debug-info binding of a source variable, in instruction order.
    /// Never deduplicated by name: a variable rebound over its lifetime has
    /// one entry per binding.
    pub(crate) dbg_values: Vec<DbgValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockIr {
    pub(crate) instrs: Vec<ValueId>,
    /// Indices into [`FunctionIr::blocks`], in the terminator's order. A
    /// block a `switch` reaches by several cases is listed once per case.
    pub(crate) successors: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValueNode {
    /// The block the instruction sits in; the entry block for a parameter.
    pub(crate) block: usize,
    pub(crate) opcode: OpcodeIr,
    /// In LLVM's operand order. A call's callee is its last operand.
    pub(crate) operands: Vec<Operand>,
    /// For a `phi`, the block each operand arrives from, in operand order.
    /// Empty for every other instruction.
    pub(crate) incoming: Vec<usize>,
    /// `Some` exactly when `opcode` is [`OpcodeIr::Call`].
    pub(crate) call: Option<CallIr>,
    pub(crate) guard: Option<GuardOpRaw>,
    /// From the instruction's `!dbg`.
    pub(crate) location: Option<SourceLocation>,
    /// Set for a `load` or `store` that is volatile or atomic: memory the
    /// program does not alone define.
    pub(crate) volatile: bool,
    /// For a `load`, the type it reads; for a `store`, the type of the value
    /// it writes. `None` for anything else, and for a type the tag does not
    /// tell apart from another.
    pub(crate) ty: Option<TypeTag>,
    /// For a `getelementptr`, the type it indexes as the IR prints it, which
    /// with its operands decides the address it computes.
    pub(crate) element: Option<String>,
}

/// A scalar type, as far as two memory accesses must agree on it: two
/// equal tags are one type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TypeTag {
    pub(crate) kind: ScalarKind,
    pub(crate) bits: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScalarKind {
    Int,
    Float,
    /// A pointer in the default address space.
    Ptr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpcodeIr {
    /// A formal parameter, not an instruction.
    Param,
    /// A `call` or an `invoke`.
    Call,
    /// A `callbr`: `asm goto`, whose effect on a value the IR does not
    /// describe.
    CallBr,
    ICmp,
    Select,
    BinOp {
        op: BinaryOp,
    },
    Cast {
        op: CastOp,
    },
    Phi,
    /// A `getelementptr`: address arithmetic, which reads no memory.
    Gep,
    Load,
    Store,
    Ret,
    /// A conditional or unconditional `br`.
    Br,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Add,
    FAdd,
    Sub,
    FSub,
    Mul,
    FMul,
    UDiv,
    SDiv,
    FDiv,
    URem,
    SRem,
    FRem,
    Shl,
    LShr,
    AShr,
    And,
    Or,
    Xor,
}

/// Kept apart because the trace treats them differently: an integer that
/// round-trips through a pointer loses what constrained it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CastOp {
    Trunc,
    ZExt,
    SExt,
    FPToUI,
    FPToSI,
    UIToFP,
    SIToFP,
    FPTrunc,
    FPExt,
    PtrToInt,
    PtrToAddr,
    IntToPtr,
    BitCast,
    AddrSpaceCast,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Operand {
    /// A parameter or instruction of the same function.
    Value(ValueId),
    Const(ConstVal),
    Func(FunctionId),
    /// Anything else: a block label, a global, a non-integer constant,
    /// `poison`, inline assembly.
    Other,
}

/// An integer constant, as the IR prints it: `16`, `-1`, `true`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConstVal {
    pub(crate) value: String,
    /// The integer type's width in bits, for rendering the value unsigned.
    pub(crate) bits: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallIr {
    /// The function called by name, intrinsics included. `None` for an
    /// indirect call and for inline assembly.
    pub(crate) callee: Option<FunctionId>,
    /// Set when the callee is inline assembly, whose effect on a value the
    /// IR does not describe.
    pub(crate) inline_asm: bool,
    /// Set when the call produces no value: its type is `void`.
    pub(crate) returns_void: bool,
    pub(crate) args: Vec<Operand>,
}

/// An instruction that constrains a value, before the analysis decides how
/// it relates to the value asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GuardOpRaw {
    pub(crate) kind: GuardKind,
    /// The operation as the IR names it: the comparison's predicate (`ult`),
    /// the clamp's direction (`umin`, `smax`), the saturating operation
    /// (`uadd.sat`) or the remainder's signedness (`urem`). `None` for a
    /// mask, which has only one reading.
    pub(crate) predicate: Option<String>,
    /// The constant operand, when there is one.
    pub(crate) bound: Option<String>,
    /// The width in bits of the integer the guard operates on.
    pub(crate) bits: Option<u32>,
}

/// An integer comparison predicate as the IR spells it.
pub(crate) struct IntPredicate {
    pub(crate) spelling: &'static str,
    /// The same comparison with its operands swapped: `ugt` for `ult`.
    pub(crate) swapped: &'static str,
    /// The comparison that holds when this one does not: `uge` for `ult`.
    pub(crate) inverse: &'static str,
    /// The clamp a choice between the two compared values makes under it:
    /// the first when it picks the left-hand value on true, the second when
    /// it picks the right. `None` for an equality, which bounds nothing.
    pub(crate) clamps: Option<[&'static str; 2]>,
}

/// Every integer predicate.
pub(crate) const INT_PREDICATES: [IntPredicate; 10] = [
    predicate("eq", "eq", "ne", None),
    predicate("ne", "ne", "eq", None),
    predicate("ugt", "ult", "ule", Some(["umax", "umin"])),
    predicate("uge", "ule", "ult", Some(["umax", "umin"])),
    predicate("ult", "ugt", "uge", Some(["umin", "umax"])),
    predicate("ule", "uge", "ugt", Some(["umin", "umax"])),
    predicate("sgt", "slt", "sle", Some(["smax", "smin"])),
    predicate("sge", "sle", "slt", Some(["smax", "smin"])),
    predicate("slt", "sgt", "sge", Some(["smin", "smax"])),
    predicate("sle", "sge", "sgt", Some(["smin", "smax"])),
];

const fn predicate(
    spelling: &'static str,
    swapped: &'static str,
    inverse: &'static str,
    clamps: Option<[&'static str; 2]>,
) -> IntPredicate {
    IntPredicate {
        spelling,
        swapped,
        inverse,
        clamps,
    }
}

/// The predicate spelled `spelling`, if it is one.
pub(crate) fn int_predicate(spelling: &str) -> Option<&'static IntPredicate> {
    INT_PREDICATES.iter().find(|row| row.spelling == spelling)
}

/// A source variable's binding from a debug record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DbgValue {
    pub(crate) name: String,
    /// Which variable it binds: the module's reader numbers each
    /// `DILocalVariable` it meets, so every record of one variable carries
    /// one number and two variables sharing a name carry two. A grouping key
    /// for this read only, never written out.
    pub(crate) variable: u32,
    /// The file the variable is declared in, as the debug info spells it,
    /// and its line there: 0 when the debug info records none.
    pub(crate) declared_file: String,
    pub(crate) declared_line: u32,
    /// What the variable holds from `at` on. [`Operand::Other`] when the
    /// record does not bind it to a value directly: it lives in memory
    /// (`#dbg_declare`), is computed from its operands (a non-empty
    /// expression or an argument list), or was optimized out (`poison`).
    pub(crate) value: Operand,
    /// The instruction the record is attached to; the binding takes effect
    /// just before it.
    pub(crate) at: ValueId,
}
