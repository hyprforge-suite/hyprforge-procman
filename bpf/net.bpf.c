// SPDX-License-Identifier: MIT OR GPL-2.0
//
// Network bytes per process, counted where the kernel hands a socket's
// data to or from the process that called: the sock_send_length and
// sock_recv_length tracepoints (Linux 6.3), which fire in the caller's
// context for every send and receive on every socket. TCP and UDP
// alike, so QUIC is counted where a TCP-only figure would miss it.
//
// Only AF_INET and AF_INET6: a Unix socket is two processes on this
// machine talking, not the network. A MSG_PEEK receive leaves the data
// queued for the receive that takes it, so it is not counted twice.
//
// The counts are since this program was loaded, by thread group (the
// process). Userspace takes differences between samples and forgets a
// process once it has gone; the map is an LRU besides, so a reader that
// stops forgetting can never fill it.
//
// No vmlinux.h: the two fields read are declared here with
// preserve_access_index, so the loader relocates them against this
// kernel's own BTF (CO-RE).

#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>

#define AF_INET 2
#define AF_INET6 10
#define MSG_PEEK 2

struct sock_common {
	unsigned short skc_family;
} __attribute__((preserve_access_index));

struct sock {
	struct sock_common __sk_common;
} __attribute__((preserve_access_index));

// Must match `Counts` in src/netbpf.rs.
struct counts {
	__u64 rx_bytes;
	__u64 tx_bytes;
	__u64 rx_calls;
	__u64 tx_calls;
};

struct {
	__uint(type, BPF_MAP_TYPE_LRU_HASH);
	__uint(max_entries, 16384);
	__type(key, __u32);
	__type(value, struct counts);
} NET_BY_PROCESS SEC(".maps");

static __always_inline int count(struct sock *sk, int ret, int flags, int sending)
{
	if (ret <= 0 || !sk)
		return 0;
	if (!sending && (flags & MSG_PEEK))
		return 0;
	unsigned short family = BPF_CORE_READ(sk, __sk_common.skc_family);
	if (family != AF_INET && family != AF_INET6)
		return 0;
	__u32 process = bpf_get_current_pid_tgid() >> 32;
	struct counts *c = bpf_map_lookup_elem(&NET_BY_PROCESS, &process);
	if (!c) {
		struct counts zero = {};
		bpf_map_update_elem(&NET_BY_PROCESS, &process, &zero, BPF_NOEXIST);
		c = bpf_map_lookup_elem(&NET_BY_PROCESS, &process);
		if (!c)
			return 0;
	}
	if (sending) {
		__sync_fetch_and_add(&c->tx_bytes, ret);
		__sync_fetch_and_add(&c->tx_calls, 1);
	} else {
		__sync_fetch_and_add(&c->rx_bytes, ret);
		__sync_fetch_and_add(&c->rx_calls, 1);
	}
	return 0;
}

// btf_trace_sock_send_length(void *, struct sock *sk, int ret, int flags):
// a tp_btf program's context is those arguments after the first.
SEC("tp_btf/sock_send_length")
int send_length(unsigned long long *ctx)
{
	return count((struct sock *)ctx[0], (int)ctx[1], (int)ctx[2], 1);
}

SEC("tp_btf/sock_recv_length")
int recv_length(unsigned long long *ctx)
{
	return count((struct sock *)ctx[0], (int)ctx[1], (int)ctx[2], 0);
}

// Tracing programs must declare a GPL-compatible licence to load.
char LICENSE[] SEC("license") = "Dual MIT/GPL";
