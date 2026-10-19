#include <cstdio>

int main() {
    int *value = new int(42);
    std::puts("Hello World!");
    int result = *value - 42;
    delete value;
    return result;
}
