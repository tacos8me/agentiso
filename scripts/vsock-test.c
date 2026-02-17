/*
 * vsock-test.c — Minimal vsock guest agent for verifying AF_VSOCK inside
 * a QEMU microvm.  Listens on vsock port 5000, accepts one connection,
 * reads a length-prefixed JSON request, prints it, and replies with a
 * Pong response using the same wire format as agentiso-guest.
 *
 * Wire format: 4-byte big-endian length prefix + JSON payload.
 *
 * Build (static, musl):
 *   musl-gcc -static -o vsock-test vsock-test.c
 *
 * Run inside the guest:
 *   ./vsock-test
 *
 * Host-side test with socat (CID is the guest's vsock CID, e.g. 3):
 *   PAYLOAD='{"type":"Ping"}'
 *   LEN=$(printf '%08x' ${#PAYLOAD})
 *   (printf "\x${LEN:0:2}\x${LEN:2:2}\x${LEN:4:2}\x${LEN:6:2}"; printf '%s' "$PAYLOAD") \
 *     | socat - VSOCK-CONNECT:3:5000 \
 *     | (head -c 4 | xxd -p; cat)
 *
 * Or, using the helper script approach:
 *   python3 -c "
 *   import struct, socket, json
 *   s = socket.socket(40, socket.SOCK_STREAM)  # AF_VSOCK=40
 *   s.connect((3, 5000))                        # CID=3, port=5000
 *   msg = json.dumps({'type':'Ping'}).encode()
 *   s.sendall(struct.pack('>I', len(msg)) + msg)
 *   raw_len = s.recv(4)
 *   resp_len = struct.unpack('>I', raw_len)[0]
 *   resp = s.recv(resp_len)
 *   print('Response:', resp.decode())
 *   s.close()
 *   "
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <stdint.h>
#include <errno.h>
#include <sys/socket.h>

/* ------------------------------------------------------------------ */
/* vsock definitions (not in musl/glibc headers on all systems)       */
/* ------------------------------------------------------------------ */

#define AF_VSOCK        40
#define VMADDR_CID_ANY  0xFFFFFFFF

struct sockaddr_vm {
    sa_family_t     svm_family;     /* AF_VSOCK = 40 */
    unsigned short  svm_reserved1;
    unsigned int    svm_port;
    unsigned int    svm_cid;
    unsigned char   svm_flags;
    unsigned char   svm_zero[3];
};

/* ------------------------------------------------------------------ */
/* helpers                                                            */
/* ------------------------------------------------------------------ */

/* Read exactly `n` bytes from `fd` into `buf`.  Returns 0 on success,
 * -1 on error or unexpected EOF. */
static int read_exact(int fd, void *buf, size_t n)
{
    size_t done = 0;
    while (done < n) {
        ssize_t r = read(fd, (char *)buf + done, n - done);
        if (r < 0) {
            if (errno == EINTR)
                continue;
            perror("read");
            return -1;
        }
        if (r == 0) {
            fprintf(stderr, "read_exact: unexpected EOF after %zu/%zu bytes\n",
                    done, n);
            return -1;
        }
        done += (size_t)r;
    }
    return 0;
}

/* Write exactly `n` bytes from `buf` to `fd`. */
static int write_all(int fd, const void *buf, size_t n)
{
    size_t done = 0;
    while (done < n) {
        ssize_t w = write(fd, (const char *)buf + done, n - done);
        if (w < 0) {
            if (errno == EINTR)
                continue;
            perror("write");
            return -1;
        }
        done += (size_t)w;
    }
    return 0;
}

/* Encode a 32-bit value in big-endian. */
static void put_be32(uint8_t *out, uint32_t val)
{
    out[0] = (uint8_t)(val >> 24);
    out[1] = (uint8_t)(val >> 16);
    out[2] = (uint8_t)(val >> 8);
    out[3] = (uint8_t)(val);
}

/* Decode a 32-bit big-endian value. */
static uint32_t get_be32(const uint8_t *in)
{
    return ((uint32_t)in[0] << 24)
         | ((uint32_t)in[1] << 16)
         | ((uint32_t)in[2] << 8)
         | ((uint32_t)in[3]);
}

/* ------------------------------------------------------------------ */
/* main                                                               */
/* ------------------------------------------------------------------ */

#define LISTEN_PORT 5000
#define MAX_MSG     (16 * 1024 * 1024)

int main(void)
{
    int sock, client;
    struct sockaddr_vm addr;
    socklen_t addr_len;

    /* 1. Create AF_VSOCK socket */
    sock = socket(AF_VSOCK, SOCK_STREAM, 0);
    if (sock < 0) {
        perror("socket(AF_VSOCK)");
        return 1;
    }
    printf("vsock-test: socket created (fd=%d)\n", sock);

    /* 2. Bind to VMADDR_CID_ANY, port 5000 */
    memset(&addr, 0, sizeof(addr));
    addr.svm_family = AF_VSOCK;
    addr.svm_port = LISTEN_PORT;
    addr.svm_cid = VMADDR_CID_ANY;

    if (bind(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("bind");
        close(sock);
        return 1;
    }
    printf("vsock-test: bound to cid=ANY port=%d\n", LISTEN_PORT);

    /* 3. Listen */
    if (listen(sock, 1) < 0) {
        perror("listen");
        close(sock);
        return 1;
    }
    printf("vsock-test: listening...\n");

    /* 4. Accept one connection */
    struct sockaddr_vm peer_addr;
    addr_len = sizeof(peer_addr);
    client = accept(sock, (struct sockaddr *)&peer_addr, &addr_len);
    if (client < 0) {
        perror("accept");
        close(sock);
        return 1;
    }
    printf("vsock-test: accepted connection from cid=%u port=%u (fd=%d)\n",
           peer_addr.svm_cid, peer_addr.svm_port, client);

    /* 5. Read 4-byte length prefix, then JSON payload */
    uint8_t len_buf[4];
    if (read_exact(client, len_buf, 4) < 0) {
        fprintf(stderr, "vsock-test: failed to read length prefix\n");
        close(client);
        close(sock);
        return 1;
    }

    uint32_t msg_len = get_be32(len_buf);
    printf("vsock-test: message length = %u bytes\n", msg_len);

    if (msg_len > MAX_MSG) {
        fprintf(stderr, "vsock-test: message too large (%u > %d)\n",
                msg_len, MAX_MSG);
        close(client);
        close(sock);
        return 1;
    }

    char *msg = malloc(msg_len + 1);
    if (!msg) {
        perror("malloc");
        close(client);
        close(sock);
        return 1;
    }

    if (read_exact(client, msg, msg_len) < 0) {
        fprintf(stderr, "vsock-test: failed to read message body\n");
        free(msg);
        close(client);
        close(sock);
        return 1;
    }
    msg[msg_len] = '\0';

    /* 6. Print what we received */
    printf("vsock-test: received: %s\n", msg);
    free(msg);

    /* 7. Write back a Pong response: 4-byte length + JSON */
    const char *response = "{\"type\":\"Pong\",\"version\":\"test\",\"uptime_secs\":0}";
    uint32_t resp_len = (uint32_t)strlen(response);
    uint8_t resp_len_buf[4];
    put_be32(resp_len_buf, resp_len);

    printf("vsock-test: sending response (%u bytes): %s\n", resp_len, response);

    if (write_all(client, resp_len_buf, 4) < 0) {
        fprintf(stderr, "vsock-test: failed to write response length\n");
        close(client);
        close(sock);
        return 1;
    }
    if (write_all(client, response, resp_len) < 0) {
        fprintf(stderr, "vsock-test: failed to write response body\n");
        close(client);
        close(sock);
        return 1;
    }

    printf("vsock-test: done, exiting.\n");

    /* 8. Clean up and exit */
    close(client);
    close(sock);
    return 0;
}
