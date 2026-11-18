/* Compile with the target compiler against the selected runtime's installed headers. */
#include <lean/lean.h>

#ifndef LEAN_MIMALLOC
#error "lean-rs-sys requires Lean built with mimalloc"
#endif

_Static_assert(sizeof(void *) == 8, "lean-rs-sys requires 64-bit pointers");
_Static_assert(sizeof(lean_object) == 8, "unsupported Lean object header");
_Static_assert(LEAN_OBJECT_SIZE_DELTA == 8, "unsupported Lean object alignment");
_Static_assert(LEAN_MAX_SMALL_OBJECT_SIZE == 4096, "unsupported Lean small object size limit");
