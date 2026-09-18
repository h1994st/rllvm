int parse(void);
int decode(void);

int main(void) { return parse() + decode() == 3 ? 0 : 1; }
