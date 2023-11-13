#include <stdio.h>
#include <netdb.h>
#include <strings.h>
#include <arpa/inet.h>

void try_gethostbyname(const char name[]) {
  struct hostent *result = gethostbyname(name);

  if (result) {
    printf("result %p\n\t", result);
    printf("h_name %s\n\t", result->h_name);
    printf("h_length %i\n\t", result->h_length);
    printf("h_addrtype %i\n\t", result->h_addrtype);

    for (int i = 0; result->h_addr_list[i]; i++) {
      char str[INET6_ADDRSTRLEN];
      struct in_addr address = {};
      bcopy(result->h_addr_list[i], (char *)&address, sizeof(address));
      printf("h_addresses[%i] %s\n\t", i, inet_ntoa(address));
    }

    for (int i = 0; result->h_aliases[i]; i++) {
      printf("h_aliases[%i] %s\n\t", i, result->h_aliases[i]);
    }
  }
}

int main(int argc, char *argv[]) {
  printf("test issue 2055: START\n");
  try_gethostbyname("www.mirrord.dev");
  try_gethostbyname("www.invalid.dev");
  printf("test issue 2055: SUCCESS\n");
  printf("\n");
}
