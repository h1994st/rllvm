struct ops {
    int tag;
    void (*on_event)(int);
};

void install(struct ops *o);
void dispatch(struct ops *o);

int main(void) {
    struct ops o = {0};
    install(&o);
    dispatch(&o);
    return 0;
}
