int helper(void);

__attribute__((visibility("default"))) int entry(void) {
    return helper() + 1;
}
