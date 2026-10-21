#include <stddef.h>

#define MAX 64

static char buf[MAX + 1];

static void make_buf(char *dst, size_t size) {
    for (size_t i = 0; i < size; i++)
        dst[i] = 0;
}

void reserve(size_t left) {
    if (left < MAX)
        make_buf(buf, left + 1);
}

int main(int argc, char **argv) {
    (void)argv;
    reserve((size_t)argc);
    return buf[0];
}
