template <typename T>
T twice(T x) {
    return x + x;
}

int main(void) { return twice(21) == 42 ? 0 : 1; }
