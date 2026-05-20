#include <stddef.h>
#include <stdint.h>

typedef uint8_t (*lean_rs_interop_callback_fn)(uintptr_t handle, uint64_t current, uint64_t total);

uint8_t lean_rs_interop_callback_call(size_t handle, size_t trampoline, uint64_t current, uint64_t total) {
    if (trampoline == 0) {
        return 1;
    }
    lean_rs_interop_callback_fn callback = (lean_rs_interop_callback_fn)(uintptr_t)trampoline;
    return callback((uintptr_t)handle, current, total);
}
