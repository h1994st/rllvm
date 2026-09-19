int wasm_helper(void);

__attribute__((visibility("default"))) int wasm_entry(void) {
    return wasm_helper() + 1;
}
