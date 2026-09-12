#include <iostream>
extern "C" int project_first(void);
extern "C" int project_second(void);
int main() { std::cout << project_first() + project_second() << '\n'; }
