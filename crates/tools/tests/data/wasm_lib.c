/* Fixture for WebAssembly tests: freestanding, no libc, two translation units
   so that linking has something to concatenate. */
int wasm_helper(void) { return 7; }
