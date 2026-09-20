int twice(int);

// Freestanding, so the example needs no cross sysroot: `_start` is the entry
// point the ELF linker looks for, and nothing here calls libc. The binary is
// never run -- it is built for a machine this host is not.
void _start(void) {
    volatile int result = twice(21);
    (void)result;
}
