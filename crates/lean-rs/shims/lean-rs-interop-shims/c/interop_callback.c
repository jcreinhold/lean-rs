#include <stddef.h>
#include <stdint.h>

typedef struct lean_object lean_object;

typedef uint8_t (*lean_rs_interop_callback_fn)(uintptr_t handle, uint64_t current, uint64_t total);
typedef uint8_t (*lean_rs_interop_string_callback_fn)(uintptr_t handle, lean_object *payload);

uint8_t lean_rs_interop_callback_call(size_t handle, size_t trampoline, uint64_t current, uint64_t total) {
    if (trampoline == 0) {
        return 1;
    }
    lean_rs_interop_callback_fn callback = (lean_rs_interop_callback_fn)(uintptr_t)trampoline;
    return callback((uintptr_t)handle, current, total);
}

uint8_t lean_rs_interop_string_callback_call(size_t handle, size_t trampoline, lean_object *payload) {
    if (trampoline == 0) {
        return 1;
    }
    lean_rs_interop_string_callback_fn callback = (lean_rs_interop_string_callback_fn)(uintptr_t)trampoline;
    return callback((uintptr_t)handle, payload);
}
