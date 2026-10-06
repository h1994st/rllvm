struct ops {
    int tag;
    void (*on_event)(int);
};

static void handler(int event) { (void)event; }

void install(struct ops *o) { o->on_event = handler; }

void dispatch(struct ops *o) { o->on_event(1); }
