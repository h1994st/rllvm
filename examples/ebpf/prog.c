/* Freestanding: no libbpf headers needed to show the capture path. */
int packets;

__attribute__((section("socket"))) int count_packet(void *context) {
    (void)context;
    packets++;
    return 0;
}
