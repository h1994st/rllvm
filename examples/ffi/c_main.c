#include <stdio.h>

int rust_scale(int x);

int c_offset(int x) { return x + 1; }

int main(void) {
    /* 20 doubled by Rust, which offsets it by calling back into C. */
    int scaled = rust_scale(20);
    printf("scaled=%d\n", scaled);
    return scaled == 41 ? 0 : 1;
}
