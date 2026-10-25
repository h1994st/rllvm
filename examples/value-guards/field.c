#include <stddef.h>

#define MAX 64

struct buffer {
    size_t len;
    char data[MAX];
};

static struct buffer out;

static void put(char *dst, const char *src, size_t n) {
    for (size_t i = 0; i < n; i++)
        dst[i] = src[i];
}

void fill(struct buffer *b, const char *src, size_t n) {
    b->len = n < MAX ? n : MAX;
    put(b->data, src, b->len);
}

int main(int argc, char **argv) {
    fill(&out, argv[0], (size_t)argc * 100);
    return out.data[0];
}
