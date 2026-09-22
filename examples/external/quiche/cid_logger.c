// Logs a connection's source IDs through quiche's FFI iterator, the usage
// pattern CVE-2026-11941 affects. Written for this example; quiche's own C
// examples never call the iterator. See README.md.
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <netinet/in.h>
#include <sys/socket.h>

#include <quiche.h>

#define LOCAL_CONN_ID_LEN 16

static void log_source_ids(quiche_conn *conn) {
    quiche_connection_id_iter *iter = quiche_conn_source_ids(conn);

    const uint8_t *cid = NULL;
    size_t cid_len = 0;

    while (quiche_connection_id_iter_next(iter, &cid, &cid_len)) {
        for (size_t i = 0; i < cid_len; i++) {
            fprintf(stderr, "%02x", cid[i]);
        }
        fprintf(stderr, "\n");
    }

    quiche_connection_id_iter_free(iter);
}

int main(void) {
    quiche_config *config = quiche_config_new(0xbabababa);
    if (config == NULL) {
        return 1;
    }

    uint8_t scid[LOCAL_CONN_ID_LEN];
    memset(scid, 0x42, sizeof(scid));

    struct sockaddr_in local, peer;
    memset(&local, 0, sizeof(local));
    memset(&peer, 0, sizeof(peer));
    local.sin_family = AF_INET;
    peer.sin_family = AF_INET;
    peer.sin_port = htons(4433);

    quiche_conn *conn =
        quiche_connect("example.com", scid, sizeof(scid),
                       (struct sockaddr *) &local, sizeof(local),
                       (struct sockaddr *) &peer, sizeof(peer), config);

    if (conn != NULL) {
        log_source_ids(conn);
        quiche_conn_free(conn);
    }

    quiche_config_free(config);
    return 0;
}
