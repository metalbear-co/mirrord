#ifdef __APPLE__

#include <arpa/inet.h>
#include <dispatch/dispatch.h>
#include <dlfcn.h>
#include <dns_sd.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>

// Resolves names through the DNS-SD client styles mirrord has to support, see `dns_sd.rs`.
//
// The test answers `*.test` lookups through the agent, `local.test` is a local DNS filter in
// `configs/dns_sd.json`.

typedef DNSServiceErrorType (*GetAddrInfoEx)(DNSServiceRef *, DNSServiceFlags, uint32_t,
                                             DNSServiceProtocol, const char *, const void *,
                                             DNSServiceGetAddrInfoReply, void *);
typedef DNSServiceErrorType (*QueryRecordWithAttribute)(DNSServiceRef *, DNSServiceFlags,
                                                        uint32_t, const char *, uint16_t,
                                                        uint16_t, const void *,
                                                        DNSServiceQueryRecordReply, void *);

// Everything a lookup received, as "address" or "error=<code>" entries.
typedef struct {
  DNSServiceRef ref;
  // Queries (families) of the lookup that haven't finished replying yet, 0 means 1.
  int remaining;
  int done;
  int dealloc_in_callback;
  char got[256];
  dispatch_semaphore_t semaphore;
} Lookup;

static int failures = 0;

static void note(Lookup *lookup, DNSServiceFlags flags, DNSServiceErrorType error,
                 const char *address) {
  char entry[64];
  if (error) {
    snprintf(entry, sizeof(entry), "error=%d", error);
  } else {
    snprintf(entry, sizeof(entry), "%s", address);
  }
  if (lookup->got[0]) {
    strlcat(lookup->got, " ", sizeof(lookup->got));
  }
  strlcat(lookup->got, entry, sizeof(lookup->got));

  if ((!(flags & kDNSServiceFlagsMoreComing) && --lookup->remaining <= 0) ||
      lookup->dealloc_in_callback) {
    lookup->done = 1;
    if (lookup->dealloc_in_callback) {
      DNSServiceRefDeallocate(lookup->ref);
    }
    if (lookup->semaphore) {
      dispatch_semaphore_signal(lookup->semaphore);
    }
  }
}

static void address_reply(DNSServiceRef sd_ref, DNSServiceFlags flags, uint32_t interface_index,
                          DNSServiceErrorType error, const char *hostname,
                          const struct sockaddr *address, uint32_t ttl, void *context) {
  char text[INET6_ADDRSTRLEN] = "";
  if (!error && address->sa_family == AF_INET) {
    inet_ntop(AF_INET, &((const struct sockaddr_in *)address)->sin_addr, text, sizeof(text));
  } else if (!error && address->sa_family == AF_INET6) {
    inet_ntop(AF_INET6, &((const struct sockaddr_in6 *)address)->sin6_addr, text, sizeof(text));
  }
  note(context, flags, error, text);
}

static void record_reply(DNSServiceRef sd_ref, DNSServiceFlags flags, uint32_t interface_index,
                         DNSServiceErrorType error, const char *fullname, uint16_t rrtype,
                         uint16_t rrclass, uint16_t rdlen, const void *rdata, uint32_t ttl,
                         void *context) {
  char text[INET6_ADDRSTRLEN] = "";
  if (!error) {
    inet_ntop(rdlen == 4 ? AF_INET : AF_INET6, rdata, text, sizeof(text));
  }
  note(context, flags, error, text);
}

// Processes `ref` until all lookups are done, like an event loop would.
static void process(DNSServiceRef ref, Lookup **lookups, int count) {
  for (;;) {
    int pending = 0;
    for (int i = 0; i < count; i++) {
      pending |= !lookups[i]->done;
    }
    if (!pending) {
      return;
    }

    struct pollfd fd = {.fd = DNSServiceRefSockFD(ref), .events = POLLIN};
    if (poll(&fd, 1, 5000) != 1) {
      printf("FAIL timed out waiting for replies\n");
      failures++;
      return;
    }
    DNSServiceProcessResult(ref);
  }
}

static int compare(const void *a, const void *b) {
  return strcmp(*(char *const *)a, *(char *const *)b);
}

// The entries of `text` in a stable order, queries of one lookup can reply in any order.
static void sort_entries(const char *text, char *out, size_t size) {
  char copy[256], *entries[16], *rest = copy, *entry;
  int count = 0;
  strlcpy(copy, text, sizeof(copy));
  while ((entry = strsep(&rest, " ")) && count < 16) {
    if (*entry) {
      entries[count++] = entry;
    }
  }
  qsort(entries, count, sizeof(char *), compare);
  out[0] = 0;
  for (int i = 0; i < count; i++) {
    if (i) {
      strlcat(out, " ", size);
    }
    strlcat(out, entries[i], size);
  }
}

static void expect(const char *what, Lookup *lookup, const char *expected) {
  char got[256], want[256];
  sort_entries(lookup->got, got, sizeof(got));
  sort_entries(expected, want, sizeof(want));
  if (strcmp(got, want)) {
    printf("FAIL %s: got \"%s\", expected \"%s\"\n", what, lookup->got, expected);
    failures++;
  } else {
    printf("ok %s: %s\n", what, lookup->got);
  }
}

// Processes a query the agent must not answer for a moment. If it was taken over anyway, its
// reply makes us ask the agent for the name, which fails the test. What the daemon answers, if
// anything, doesn't matter.
static void expect_passed_through(const char *what, DNSServiceErrorType error, DNSServiceRef ref) {
  if (error) {
    printf("FAIL %s: not passed to the daemon (%d)\n", what, error);
    failures++;
    return;
  }

  struct pollfd fd = {.fd = DNSServiceRefSockFD(ref), .events = POLLIN};
  for (int i = 0; i < 10 && poll(&fd, 1, 500) == 1; i++) {
    if (DNSServiceProcessResult(ref)) {
      break;
    }
  }

  printf("ok %s: passed to the daemon\n", what);
  DNSServiceRefDeallocate(ref);
}

int main(int argc, char *argv[]) {
  printf("test dns_sd: START\n");

  GetAddrInfoEx get_addr_info_ex =
      (GetAddrInfoEx)dlsym(RTLD_DEFAULT, "DNSServiceGetAddrInfoEx");
  QueryRecordWithAttribute query_record_with_attribute =
      (QueryRecordWithAttribute)dlsym(RTLD_DEFAULT, "DNSServiceQueryRecordWithAttribute");
  if (!get_addr_info_ex || !query_record_with_attribute) {
    printf("FAIL DNSServiceGetAddrInfoEx or DNSServiceQueryRecordWithAttribute missing\n");
    return 1;
  }

  DNSServiceFlags shared = kDNSServiceFlagsShareConnection | kDNSServiceFlagsTimeout;
  DNSServiceProtocol both = kDNSServiceProtocol_IPv4 | kDNSServiceProtocol_IPv6;

  // Queries the agent doesn't answer go to the daemon untouched. These run first, so a wrongly
  // taken over one is the first (unexpected) name the test sees.
  DNSServiceRef ref;
  DNSServiceErrorType error;
  Lookup ignored = {0};

  error = DNSServiceGetAddrInfo(&ref, 0, 0, both, "local.test", address_reply, &ignored);
  expect_passed_through("local DNS filter", error, ref);

  error = DNSServiceGetAddrInfo(&ref, 0, 0, both, "local.test.", address_reply, &ignored);
  expect_passed_through("local DNS filter, fully qualified", error, ref);

  error = DNSServiceQueryRecord(&ref, 0, 0, "remote.test", kDNSServiceType_TXT,
                                kDNSServiceClass_IN, record_reply, &ignored);
  expect_passed_through("TXT record", error, ref);

  error = DNSServiceGetAddrInfo(&ref, 0, kDNSServiceInterfaceIndexLocalOnly, both, "remote.test",
                                address_reply, &ignored);
  expect_passed_through("interface index", error, ref);

  error = DNSServiceGetAddrInfo(&ref, 0, 0, both, "10.1.1.1", address_reply, &ignored);
  expect_passed_through("IP literal", error, ref);

  DNSServiceRef connection;
  DNSServiceCreateConnection(&connection);

  // Deallocated before its reply is processed: never resolved.
  Lookup cancelled = {.ref = connection};
  DNSServiceQueryRecord(&cancelled.ref, shared, 0, "cancelled.test", kDNSServiceType_A,
                        kDNSServiceClass_IN, record_reply, &cancelled);
  DNSServiceRefDeallocate(cancelled.ref);

  // Bun main: one lookup asks for A and AAAA with two queries on a shared connection, sharing a
  // callback and context, and is resolved once.
  Lookup lookup = {.remaining = 2};
  DNSServiceRef a = connection, aaaa = connection;
  DNSServiceQueryRecord(&a, shared, 0, "remote.test", kDNSServiceType_A, kDNSServiceClass_IN,
                        record_reply, &lookup);
  query_record_with_attribute(&aaaa, shared, 0, "remote.test", kDNSServiceType_AAAA,
                              kDNSServiceClass_IN, NULL, record_reply, &lookup);
  process(connection, (Lookup *[]){&lookup}, 1);
  expect("QueryRecord(A) + QueryRecordWithAttribute(AAAA) shared", &lookup,
         "10.0.0.1 error=-65554");
  DNSServiceRefDeallocate(a);
  DNSServiceRefDeallocate(aaaa);

  // Bun 1.4.2: GetAddrInfoEx on a shared connection.
  lookup = (Lookup){.ref = connection};
  get_addr_info_ex(&lookup.ref, shared, 0, both, "ex.test", NULL, address_reply, &lookup);
  process(connection, (Lookup *[]){&lookup}, 1);
  expect("GetAddrInfoEx shared", &lookup, "10.0.0.2 error=-65554");
  DNSServiceRefDeallocate(lookup.ref);

  // A standalone ref, deallocated in its first callback: no more callbacks after that.
  lookup = (Lookup){.dealloc_in_callback = 1};
  DNSServiceGetAddrInfo(&lookup.ref, kDNSServiceFlagsTimeout, 0, both, "standalone.test",
                        address_reply, &lookup);
  process(lookup.ref, (Lookup *[]){&lookup}, 1);
  expect("GetAddrInfo standalone, deallocated in callback", &lookup, "10.0.0.3");

  // Delivery on a dispatch queue, deallocated on that queue.
  lookup = (Lookup){.dealloc_in_callback = 1, .semaphore = dispatch_semaphore_create(0)};
  DNSServiceGetAddrInfo(&lookup.ref, kDNSServiceFlagsTimeout, 0, kDNSServiceProtocol_IPv4,
                        "dispatch.test", address_reply, &lookup);
  DNSServiceSetDispatchQueue(lookup.ref, dispatch_queue_create("dns_sd", NULL));
  if (dispatch_semaphore_wait(lookup.semaphore,
                              dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC))) {
    printf("FAIL timed out waiting for dispatch queue replies\n");
    failures++;
  }
  expect("GetAddrInfo on a dispatch queue", &lookup, "10.0.0.4");

  // The agent finds no records.
  lookup = (Lookup){.ref = connection};
  query_record_with_attribute(&lookup.ref, shared, 0, "missing.test", kDNSServiceType_A,
                              kDNSServiceClass_IN, NULL, record_reply, &lookup);
  process(connection, (Lookup *[]){&lookup}, 1);
  expect("QueryRecordWithAttribute(A) no records", &lookup, "error=-65554");
  DNSServiceRefDeallocate(lookup.ref);

  DNSServiceRefDeallocate(connection);

  if (failures) {
    printf("test dns_sd: FAILED (%d)\n", failures);
    return 1;
  }

  printf("test dns_sd: SUCCESS\n");
  return 0;
}

#else

int main(int argc, char *argv[]) { return 0; }

#endif
