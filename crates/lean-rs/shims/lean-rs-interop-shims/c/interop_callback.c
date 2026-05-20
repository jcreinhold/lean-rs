#include <stddef.h>
#include <stdint.h>

typedef struct lean_object lean_object;

typedef uint8_t (*lean_rs_interop_callback_fn)(uintptr_t handle, uint8_t payload_tag, uint64_t arg0, uint64_t arg1, lean_object *payload);

enum {
    LEAN_RS_INTEROP_PAYLOAD_TICK = 0,
    LEAN_RS_INTEROP_PAYLOAD_STRING = 1,
};

uint8_t lean_rs_interop_tick_callback_call(size_t handle, size_t trampoline, uint64_t current, uint64_t total) {
    if (trampoline == 0) {
        return 1;
    }
    lean_rs_interop_callback_fn callback = (lean_rs_interop_callback_fn)(uintptr_t)trampoline;
    return callback((uintptr_t)handle, LEAN_RS_INTEROP_PAYLOAD_TICK, current, total, NULL);
}

uint8_t lean_rs_interop_string_callback_call(size_t handle, size_t trampoline, lean_object *payload) {
    if (trampoline == 0) {
        return 1;
    }
    lean_rs_interop_callback_fn callback = (lean_rs_interop_callback_fn)(uintptr_t)trampoline;
    return callback((uintptr_t)handle, LEAN_RS_INTEROP_PAYLOAD_STRING, 0, 0, payload);
}
