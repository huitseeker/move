// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

use maplit::btreemap;
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::BTreeMap;

/// The word size (in bytes) of the EVM.
pub const WORD_SIZE: usize = 32;

/// A lazy constant which defines placeholders which can be referenced as `${NAME}`
/// in emitted code. All emitted strings have those placeholders substituted.
static PLACEHOLDERS: Lazy<BTreeMap<&'static str, &'static str>> = Lazy::new(|| {
    btreemap! {
        // ---------------------------------
        // Numerical constants
        "MAX_U8" => "0xff",
        "MAX_U64" => "0xffffffffffffffff",
        "MAX_U128" => "0xffffffffffffffffffffffffffffffff",
        "MAX_U256" =>
        "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        "ADDRESS_U160" => "0xffffffffffffffffffffffffffffffffffffffff",

        // ---------------------------------
        // Memory
        // The size of the memory used by the compilation scheme. This must be the
        // sum of the sizes required by the locations defined below.
        "USED_MEM" => "160",

        // Location where the current size of the used memory is stored. New memory will
        // be allocated from there.
        "MEM_SIZE_LOC" => "0",

        // Locations in memory we use for scratch computations
        "SCRATCH1_LOC" => "32",
        "SCRATCH2_LOC" => "64",

        // Storage groups. Those are used to augment words by creating a keccak256 value from
        // word and group to create a unique storage key. This basically allows -- by the
        // magic if keccak -- to multiplex the 256 bit address space into multiple ones, and
        // to implement tables with 256 bit keys. The LINEAR_STORAGE_GROUP is reserved
        // for Move memory. Other groups are created as tables are dynamically allocated.
        // STORAGE_GROUP_COUNTER_LOC contains the largest storage group allocated so far.
        // A storage group identifier is 4 bytes long.
        "LINEAR_STORAGE_GROUP" => "0",
        "STORAGE_GROUP_COUNTER_LOC" => "96",
        "WORD_AND_STORAGE_GROUP_LENGTH" => "36",

        // Categories to distinguish different types of pointers into the LINEAR_STORAGE_GROUP.
        // See discussion of YulFunction::MakeTypeStorageOffset.
        "RESOURCE_STORAGE_CATEGORY" => "0",
        "LINKED_STORAGE_CATEGORY" => "1",
        "LINKED_STORAGE_COUNTER_LOC" => "128",

        // Size (in bytes) of the resource exists flag which proceeds any data in storage for
        // a resource.
        "RESOURCE_EXISTS_FLAG_SIZE" => "32",

    }
});

/// Substitutes placeholders in the given string.
pub fn substitute_placeholders(s: &str) -> Option<String> {
    static REX: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)(\$\{(?P<var>[A-Z0-9_]+)\})").unwrap());
    let mut at = 0;
    let mut changes = false;
    let mut res = "".to_string();
    while let Some(cap) = (*REX).captures(&s[at..]) {
        let m = cap.get(0).unwrap();
        let v = cap.name("var").unwrap();
        res.push_str(&s[at..at + m.start()]);
        if let Some(repl) = PLACEHOLDERS.get(v.as_str()) {
            changes = true;
            res.push_str(repl)
        } else {
            res.push_str(m.as_str())
        }
        at += m.end();
    }
    if changes {
        res.push_str(&s[at..]);
        Some(res)
    } else {
        None
    }
}

/// A macro which allows to define Yul functions together with their definitions.
/// This generates an enum `YulFunction` and functions `yule_name`, `yul_def`,
/// and `yul_deps` for values of this type.
macro_rules! functions {
    ($($name:ident: $def:literal $(dep $dep:ident)*),* $(, )?) => {
        #[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug, Hash)]
        #[allow(dead_code)]
        pub enum YulFunction {
            $($name,)*
        }
        impl YulFunction {
            #[allow(dead_code)]
            pub fn yule_name(self) -> String {
                match self {
                $(
                    YulFunction::$name => make_yule_name(stringify!($name)),
                )*
                }
            }
            #[allow(dead_code)]
            pub fn yule_def(self) -> String {
                match self {
                $(
                    YulFunction::$name => make_yule_def(stringify!($name), $def),
                )*
                }
            }
            #[allow(dead_code)]
            pub fn yule_deps(self) -> Vec<YulFunction> {
                match self {
                $(
                    YulFunction::$name => vec![$(YulFunction::$dep,)*],
                )*
                }

            }
        }
    }
}

/// Helper to create name of Yul function.
fn make_yule_name(name: &str) -> String {
    format!("${}", name)
}

/// Helper to create definition of a Yule function.
fn make_yule_def(name: &str, body: &str) -> String {
    format!("function ${}{}", name, body)
}

// The Yul functions supporting the compilation scheme.
functions! {
// -------------------------------------------------------------------------------------------
// Abort
Abort: "(code) {
    mstore(0, code)
    revert(24, 8) // TODO: store code as a string?
}",
AbortBuiltin: "() {
    $Abort(sub(0, 1))
}" dep Abort,
NotImplemented: "() {
    $AbortBuiltin()
}" dep AbortBuiltin,

// -------------------------------------------------------------------------------------------
// Memory

// TODO: many of the memory operations which take a `size` parameter can be specialized
//   for the given size (8, 64, 128, or 256 bytes). The Yul optimizer does some of this,
//   but it is not transparent how far this goes. We should better generate those
//   functions algorithmically and specialize them ourselves. Doing the specialization
//   manual is too error prone.

// Allocates memory of size.
// TODO: add some memory recovery (e.g. over free lists), and benchmark against the current
//   arena style.
Malloc: "(size) -> offs {
    offs := mload(${MEM_SIZE_LOC})
    // pad to word size
    mstore(${MEM_SIZE_LOC}, add(offs, shl(5, shr(5, add(size, 31)))))
}",

// Frees memory of size
Free: "(offs, size) {
}",

// Makes a pointer, using the lowest bit to indicate whether it is for storage or memory.
MakePtr: "(is_storage, offs) -> ptr {
  ptr := or(is_storage, shl(1, offs))
}",

// Returns true if this is a storage  pointer.
IsStoragePtr: "(ptr) -> b {
  b := and(ptr, 0x1)
}",

// Returns the offset of this pointer.
OffsetPtr: "(ptr) -> offs {
  offs := shr(1, ptr)
}",

// Constructs a bit mask for a value of size bytes. E.g. if size == 1, returns 0xff.
// Note that we expect the Yul optimizer to specialize this for constant parameters.
MaskForSize: "(size) -> mask {
  mask := sub(shl(shl(size, 3), 1), 1)
}",

// Extracts size bytes from word, starting at byte index start. The most significant byte
// is at index 0 (big endian).
ExtractBytes: "(word, start, size) -> bytes {
   switch size
   case 1 {
      // use the faster byte primitive
      bytes := byte(start, word)
   }
   default {
      // As we have big endian, we need to right shift the value from
      // where the highest byte starts in the word (32 - start), minus
      // the size.
      let shift_bits := shl(3, sub(sub(32, start), size))
      bytes := and(shr(shift_bits, word), $MaskForSize(size))
   }
}" dep MaskForSize,

// Inject size bytes into word, starting a byte index start.
InjectBytes: "(word, start, size, bytes) -> new_word {
   let shift_bits := shl(3, sub(sub(32, start), size))
   // Blend out the bits which we inject
   let neg_mask := not(shl(shift_bits, $MaskForSize(size)))
   word := and(word, neg_mask)
   // Overlay the bits we inject
   new_word := or(word, shl(shift_bits, bytes))
}" dep MaskForSize,

// For a byte offset, compute word offset and byte offset within this word.
ToWordOffs: "(offs) -> word_offs, byte_offset {
  word_offs := shr(5, offs)
  byte_offset := and(offs, 0x1F)
}",

// For a byte offset within a word (< 32), compute the number of bytes which
// overflow the word for a value of size.
OverflowBytes: "(byte_offset, size) -> overflow_bytes {
  let available_bytes := sub(32, byte_offset)
  switch gt(size, available_bytes)
  case 0 {
    overflow_bytes := 0
  }
  default {
    overflow_bytes := sub(size, available_bytes)
  }
}",

// Loads bytes from memory offset.
MemoryLoadBytes: "(offs, size) -> val {
  // Lower bit where the value in the higher bytes ends
  let bit_end := shl(3, sub(32, size))
  val := shr(bit_end, mload(offs))
}" dep MaskForSize,

// Stores bytes to memory offset.
MemoryStoreBytes: "(offs, size, val) {
  let bit_end := shl(3, sub(32, size))
  let mask := shl(bit_end, $MaskForSize(size))
  mstore(offs, or(and(mload(offs), not(mask)), shl(bit_end, val)))
}" dep MaskForSize,

// Loads bytes from storage offset.
StorageLoadBytes: "(offs, size) -> val {
  let word_offs, byte_offs := $ToWordOffs(offs)
  let key := $StorageKey(${LINEAR_STORAGE_GROUP}, word_offs)
  val := $ExtractBytes(sload(key), byte_offs, size)
  let overflow_bytes := $OverflowBytes(byte_offs, size)
  if not(iszero(overflow_bytes)) {
    key := $StorageKey(${LINEAR_STORAGE_GROUP}, add(word_offs, 1))
    let extra_bytes := $ExtractBytes(sload(key), 0, overflow_bytes)
    val := or(shl(shl(3, overflow_bytes), val), extra_bytes)
  }
}" dep ToWordOffs dep StorageKey dep ExtractBytes dep OverflowBytes,

// Store bytes to storage offset.
StorageStoreBytes: "(offs, size, bytes) {
  let word_offs, byte_offs := $ToWordOffs(offs)
  let key := $StorageKey(${LINEAR_STORAGE_GROUP}, word_offs)
  let overflow_bytes := $OverflowBytes(byte_offs, size)
  switch overflow_bytes
  case 0 {
    sstore(key, $InjectBytes(sload(key), byte_offs, size, bytes))
  }
  default {
    // Shift the higher bytes to the right
    let used_bytes := sub(size, overflow_bytes)
    let higher_bytes := shr(used_bytes, bytes)
    let lower_bytes := and(bytes, $MaskForSize(overflow_bytes))
    sstore(key, $InjectBytes(sload(key), byte_offs, used_bytes, higher_bytes))
    key := $StorageKey(${LINEAR_STORAGE_GROUP}, add(word_offs, 1))
    sstore(key, $InjectBytes(sload(key), 0, overflow_bytes, lower_bytes))
  }
}" dep ToWordOffs dep StorageKey dep InjectBytes dep OverflowBytes,

// Make a unique key into storage, where word can have full 32 byte size, and type
// indicates the kind of the key given as a byte. This uses keccak256 to fold
// value and type into a unique storage key.
StorageKey: "(group, word) -> key {
  mstore(${SCRATCH1_LOC}, word)
  mstore(${SCRATCH2_LOC}, group)
  key := keccak256(${SCRATCH1_LOC}, ${WORD_AND_STORAGE_GROUP_LENGTH})
}",

// Make a base storage offset for a given type. The result has 255 bits width and can be passed into
// $MakePtr(true, result) to create a pointer. This pointer can be used to linearly address
// exclusive memory, owned by the pointer, with an address space of 60 bits.
//
//  254                                                    0
//  cccccc..cccccctttttt..tttttiiiii..iiiiiioooooo..oooooooo
//   category       type_hash     id           offset
//      3              32         160           60
//
// The category indicates what kind of type storage this is, and determines how id
// is interpreted. RESOURCE_STORAGE_CATEGORY indicates that id is a resource
// address. LINKED_STORAGE_CATEGORY indicates that id is a handle for data linked
// to from some other storage (for instance, a vector aggregated by a resource).
// The type_hash identifies the type of the stored value. The id is any 20 byte
// number which identifies an instance of this type (e.g. an address if this is a resource).
MakeTypeStorageBase: "(category, type_hash, id) -> offs {
  offs := or(shl(252, category), or(shl(220, type_hash), shl(60, id)))
}",

// Make a new base storage offset for linked storage. This creates a new handle
// and the calls MakeTypeStorageBase.
NewLinkedStorageBase: "(type_hash) -> offs {
  let handle := mload(${LINKED_STORAGE_COUNTER_LOC})
  mstore(${LINKED_STORAGE_COUNTER_LOC}, add(handle, 1))
  offs := $MakeTypeStorageBase(${LINKED_STORAGE_CATEGORY}, type_hash, handle)
}" dep MakeTypeStorageBase,

// Indexes pointer by offset.
IndexPtr: "(ptr, offs) -> new_ptr {
  new_ptr := $MakePtr($IsStoragePtr(ptr), add($OffsetPtr(ptr), offs))
}" dep MakePtr dep IsStoragePtr dep OffsetPtr,

// ------------

// Loads u8 from pointer.
LoadU8: "(ptr) -> val {
  let offs := $OffsetPtr(ptr)
  switch $IsStoragePtr(ptr)
  case 0 {
    val := $MemoryLoadU8(offs)
  }
  default {
    val := $StorageLoadU8(offs)
  }
}" dep OffsetPtr dep IsStoragePtr dep MemoryLoadU8 dep StorageLoadU8,

// Loads u8 from memory offset.
MemoryLoadU8: "(offs) -> val {
  val := $MemoryLoadBytes(offs, 1)
}" dep MemoryLoadBytes,

// Loads u8 from storage offset.
StorageLoadU8: "(offs) -> val {
  val := $StorageLoadBytes(offs, 1)
}" dep StorageLoadBytes,

// Stores u8 to pointer.
StoreU8: "(ptr, val) {
  let offs := $OffsetPtr(ptr)
  switch $IsStoragePtr(ptr)
  case 0 {
    $MemoryStoreU8(offs, val)
  }
  default {
    $StorageStoreU8(offs, val)
  }
}" dep OffsetPtr dep IsStoragePtr dep MemoryStoreU8 dep StorageStoreU8,

// Stores u8 to memory offset.
MemoryStoreU8: "(offs, val) {
  // Shortcut via special instruction
  mstore8(offs, val)
}",

// Stores u8 to storage offset.
StorageStoreU8: "(offs, val) {
  $StorageStoreBytes(offs, 1, val)
}" dep StorageStoreBytes,

// ------------

// Loads u64 from pointer.
LoadU64: "(ptr) -> val {
  let offs := $OffsetPtr(ptr)
  switch $IsStoragePtr(ptr)
  case 0 {
    val := $MemoryLoadU64(offs)
  }
  default {
    val := $StorageLoadU64(offs)
  }
}" dep OffsetPtr dep IsStoragePtr dep MemoryLoadU64 dep StorageLoadU64,

// Loads u64 from memory offset.
MemoryLoadU64: "(offs) -> val {
  val := $MemoryLoadBytes(offs, 8)
}" dep MemoryLoadBytes,

// Loads u64 from storage offset.
StorageLoadU64: "(offs) -> val {
  val := $StorageLoadBytes(offs, 8)
}" dep StorageLoadBytes,

// Stores u64 to pointer.
StoreU64: "(ptr, val) {
  let offs := $OffsetPtr(ptr)
  switch $IsStoragePtr(ptr)
  case 0 {
    $MemoryStoreU64(offs, val)
  }
  default {
    $StorageStoreU64(offs, val)
  }
}" dep OffsetPtr dep IsStoragePtr dep MemoryStoreU64 dep StorageStoreU64,

// Stores u64 to memory offset.
MemoryStoreU64: "(offs, val) {
  $MemoryStoreBytes(offs, 8, val)
}" dep MemoryStoreBytes,

// Stores u64 to storage offset.
StorageStoreU64: "(offs, val) {
  $StorageStoreBytes(offs, 8, val)
}" dep StorageStoreBytes,

// ------------

// Loads u128 from pointer.
LoadU128: "(ptr) -> val {
  let offs := $OffsetPtr(ptr)
  switch $IsStoragePtr(ptr)
  case 0 {
    val := $MemoryLoadU128(offs)
  }
  default {
    val := $StorageLoadU128(offs)
  }
}" dep OffsetPtr dep IsStoragePtr dep MemoryLoadU128 dep StorageLoadU128,

// Loads u128 from memory offset.
MemoryLoadU128: "(offs) -> val {
  val := $MemoryLoadBytes(offs, 16)
}" dep MemoryLoadBytes,

// Loads u128 from storage offset.
StorageLoadU128: "(offs) -> val {
  val := $StorageLoadBytes(offs, 16)
}" dep StorageLoadBytes,

// Stores u128 to pointer.
StoreU128: "(ptr, val) {
  let offs := $OffsetPtr(ptr)
  switch $IsStoragePtr(ptr)
  case 0 {
    $MemoryStoreU128(offs, val)
  }
  default {
    $StorageStoreU128(offs, val)
  }
}" dep OffsetPtr dep IsStoragePtr dep MemoryStoreU128 dep StorageStoreU128,

// Stores u128 to memory offset.
MemoryStoreU128: "(offs, val) {
  $MemoryStoreBytes(offs, 16, val)
}" dep MemoryStoreBytes,

// Stores u128 to storage offset.
StorageStoreU128: "(offs, val) {
  $StorageStoreBytes(offs, 16, val)
}" dep StorageStoreBytes,

// ------------

// Loads u256 from pointer.
LoadU256: "(ptr) -> val {
  let offs := $OffsetPtr(ptr)
  switch $IsStoragePtr(ptr)
  case 0 {
    val := $MemoryLoadU256(offs)
  }
  default {
    val := $StorageLoadU256(offs)
  }
}" dep OffsetPtr dep IsStoragePtr dep MemoryLoadU256 dep StorageLoadU256,

// Loads u256 from memory offset.
MemoryLoadU256: "(offs) -> val {
  val := $MemoryLoadBytes(offs, 32)
}" dep MemoryLoadBytes,

// Loads u256 from storage offset.
StorageLoadU256: "(offs) -> val {
  val := $StorageLoadBytes(offs, 32)
}" dep StorageLoadBytes,

// Stores u256 to pointer.
StoreU256: "(ptr, val) {
  let offs := $OffsetPtr(ptr)
  switch $IsStoragePtr(ptr)
  case 0 {
    $MemoryStoreU256(offs, val)
  }
  default {
    $StorageStoreU256(offs, val)
  }
}" dep OffsetPtr dep IsStoragePtr dep MemoryStoreU256 dep StorageStoreU256,

// Stores u256 to memory offset.
MemoryStoreU256: "(offs, val) {
  $MemoryStoreBytes(offs, 32, val)
}" dep MemoryStoreBytes,

// Stores u256 to storage offset.
StorageStoreU256: "(offs, val) {
  $StorageStoreBytes(offs, 32, val)
}" dep StorageStoreBytes,

// ------------

// Loads u256 from a word-aligned storage offset
AlignedStorageLoad: "(offs) -> val {
  let word_offs := shr(5, offs)
  val := sload($StorageKey(${LINEAR_STORAGE_GROUP}, word_offs))
}" dep StorageKey,

// Stores u256 to a word-aligned storage offset
AlignedStorageStore: "(offs, val) {
  let word_offs := shr(5, offs)
  sstore($StorageKey(${LINEAR_STORAGE_GROUP}, word_offs), val)
}" dep StorageKey,

// Copies size bytes from memory to memory.
CopyMemory: "(src, dst, size) {
  let i := 0
  for { } and(lt(i, length), gt(i, 31)) { i := add(i, 32) } {
    mstore(add(dst, i), mload(add(src, i)))
  }
  if lt(i, length) {
    let mask := sub(shl(shl(3, i), 1), 1)
    let dst_word := and(mload(add(dst, i)), not(mask))
    let src_word := and(mload(add(src, i)), mask)
    mstore(add(dst, i), or(dst_word, src_word))
  }
}",

// -------------------------------------------------------------------------------------------
// Arithmetic, Logic, and Relations
AddU64: "(x, y) -> r {
    if lt(sub(${MAX_U64}, x), y) { $AbortBuiltin() }
    r := add(x, y)
}" dep AbortBuiltin,
MulU64: "(x, y) -> r {
    if gt(y, div(${MAX_U64}, x)) { $AbortBuiltin() }
    r := mul(x, y)
}" dep AbortBuiltin,
AddU8: "(x, y) -> r {
    if lt(sub(${MAX_U8}, x), y) { $AbortBuiltin() }
    r := add(x, y)
}" dep AbortBuiltin,
MulU8: "(x, y) -> r {
    if gt(y, div(${MAX_U8}, x)) { $AbortBuiltin() }
    r := mul(x, y)
}" dep AbortBuiltin,
AddU128: "(x, y) -> r {
    if lt(sub(${MAX_U128}, x), y) { $AbortBuiltin() }
    r := add(x, y)
}" dep AbortBuiltin,
MulU128: "(x, y) -> r {
    if gt(y, div(${MAX_U128}, x)) { $AbortBuiltin() }
    r := mul(x, y)
}" dep AbortBuiltin,
AddU256: "(x, y) -> r {
    if lt(sub(${MAX_U256}, x), y) { $AbortBuiltin() }
    r := add(x, y)
}" dep AbortBuiltin,
MulU256: "(x, y) -> r {
    if gt(y, div(${MAX_U256}, x)) { $AbortBuiltin() }
    r := mul(x, y)
}" dep AbortBuiltin,
Sub: "(x, y) -> r {
    if lt(x, y) { $AbortBuiltin() }
    r := sub(x, y)
}" dep AbortBuiltin,
Div: "(x, y) -> r {
    if eq(y, 0) { $AbortBuiltin() }
    r := div(x, y)
}" dep AbortBuiltin,
Mod: "(x, y) -> r {
    if eq(y, 0) { $AbortBuiltin() }
    r := mod(x, y)
}" dep AbortBuiltin,
Shr: "(x, y) -> r {
    r := shr(y, x)
}",
ShlU8: "(x, y) -> r {
    r := and(shl(y, x), ${MAX_U8})
}",
ShlU64: "(x, y) -> r {
    r := and(shl(y, x), ${MAX_U64})
}",
ShlU128: "(x, y) -> r {
    r := and(shl(y, x), ${MAX_U128})
}",
ShlU256: "(x, y) -> r {
    r := and(shl(y, x), ${MAX_U256})
}",
Gt: "(x, y) -> r {
    r := gt(x, y)
}",
Lt: "(x, y) -> r {
    r := lt(x, y)
}",
GtEq: "(x, y) -> r {
    r := or(gt(x, y), eq(x, y))
}",
LtEq: "(x, y) -> r {
    r := or(lt(x, y), eq(x, y))
}",
Eq: "(x, y) -> r {
    r := eq(x, y)
}",
Neq: "(x, y) -> r {
    r := not(eq(x, y))
}",
LogicalAnd: "(x, y) -> r {
    r := and(x, y)
}",
LogicalOr: "(x, y) -> r {
    r := or(x, y)
}",
LogicalNot: "(x) -> r {
    r := not(x)
}",
BitAnd: "(x, y) -> r {
    r := and(x, y)
}",
BitOr: "(x, y) -> r {
    r := or(x, y)
}",
BitXor: "(x, y) -> r {
    r := xor(x, y)
}",
BitNot: "(x) -> r {
    r := not(x)
}",
CastU8: "(x) -> r {
    if gt(x, ${MAX_U8}) { $AbortBuiltin() }
    r := x
}" dep AbortBuiltin,
CastU64: "(x) -> r {
    if gt(x, ${MAX_U64}) { $AbortBuiltin() }
    r := x
}" dep AbortBuiltin,
CastU128: "(x) -> r {
    if gt(x, ${MAX_U128}) { $AbortBuiltin() }
    r := x
}" dep AbortBuiltin,
CastU256: "(hi, lo) -> r {
    if gt(hi, ${MAX_U128}) { $AbortBuiltin() }
    if gt(lo, ${MAX_U128}) { $AbortBuiltin() }
    r := add(shl(128, hi), lo)
}" dep AbortBuiltin,
}
