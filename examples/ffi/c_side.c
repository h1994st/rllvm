int rust_add(int a, int b);

int c_double(int x) { return rust_add(x, x); }
