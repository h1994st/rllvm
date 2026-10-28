#include <stddef.h>

static char out[64];

static void put(size_t n) {
    out[n] = 1;
}

void fill(size_t head, size_t tail) {
    size_t cur = head;
    if (cur > 32)
        return;
    put(cur);
    cur = tail;
    if (cur > 8)
        return;
    put(cur);
}

void drain(size_t head, size_t tail) {
    size_t cur = head;
    if (cur > 32)
        return;
    {
        size_t cur = tail;
        if (cur > 8)
            return;
        put(cur);
    }
    put(cur);
}

int main(int argc, char **argv) {
    (void)argv;
    fill((size_t)argc, (size_t)argc * 2);
    drain((size_t)argc, (size_t)argc * 2);
    return out[0];
}
