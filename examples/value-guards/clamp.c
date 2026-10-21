#include <stddef.h>

#define CAP 64

static char buf[CAP];

static void put(char *dst, const char *src, size_t n) {
    for (size_t i = 0; i < n; i++)
        dst[i] = src[i];
}

void copy(char *dst, const char *src, size_t len) {
    size_t n = len < CAP ? len : CAP;
    put(dst, src, n);
}

int main(int argc, char **argv) {
    copy(buf, argv[0], (size_t)argc * 100);
    return buf[0];
}

void peek(char *dst, const char *src, size_t len) {
    size_t n;
    if (len < CAP) {
        n = len;
        put(dst, src, len);
    } else {
        n = CAP;
    }
    put(dst, src, n);
}
