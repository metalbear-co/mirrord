#include <arpa/inet.h>
#include <ifaddrs.h>
#include <net/if.h>
#include <netinet/in.h>
#include <stdio.h>
#include <sys/socket.h>

/* Read one byte through a volatile lvalue: the access is a mandated side effect. */
static inline unsigned char vread(const void *p) {
    return *(const volatile unsigned char *)p;
}

/* Touch every byte of a sockaddr through volatile reads (freed-memory probe). */
static void touch_sockaddr(const struct sockaddr *sa) {
    if (sa == NULL) {
        return;
    }

    sa_family_t fam = ((const volatile struct sockaddr *)sa)->sa_family;
    size_t len;
    switch (fam) {
    case AF_INET:
        len = sizeof(struct sockaddr_in);
        break;
    case AF_INET6:
        len = sizeof(struct sockaddr_in6);
        break;
    default:
        len = sizeof(struct sockaddr);
        break;
    }

    for (size_t i = 0; i < len; i++) {
        vread((const unsigned char *)sa + i);
    }
}

static const char *family_token(const struct sockaddr *sa) {
    if (sa == NULL) {
        return "none";
    }
    switch (sa->sa_family) {
    case AF_INET:
        return "IPv4";
    case AF_INET6:
        return "IPv6";
    default:
        return "other";
    }
}

int main(void) {
    struct ifaddrs *head = NULL;

    if (getifaddrs(&head) != 0) {
        perror("getifaddrs");
        return 1;
    }

    int n = 0;
    for (struct ifaddrs *ifa = head; ifa != NULL; ifa = ifa->ifa_next) {
        n++;

        /* Probe every inner pointer via volatile reads. */
        if (ifa->ifa_name != NULL) {
            for (size_t i = 0; vread((const unsigned char *)ifa->ifa_name + i) != 0; i++) {
            }
        }
        touch_sockaddr(ifa->ifa_addr);
        touch_sockaddr(ifa->ifa_netmask);
        touch_sockaddr(ifa->ifa_broadaddr); /* ifa_ifu union */
        if (ifa->ifa_data != NULL) {
            for (size_t i = 0; i < 16; i++) {
                vread((const unsigned char *)ifa->ifa_data + i);
            }
        }

        printf("interface name=%s family=%s\n",
               ifa->ifa_name ? ifa->ifa_name : "(null)",
               family_token(ifa->ifa_addr));
    }

    printf("walked %d interfaces\n", n);

    freeifaddrs(head);
    return 0;
}
