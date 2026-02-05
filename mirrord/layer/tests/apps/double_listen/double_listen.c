#include <stdio.h>
#include <unistd.h>
#include <arpa/inet.h>
#include <stdlib.h>

int main() {
	int s = socket(AF_INET, SOCK_STREAM, 0);
	if (s < 0) { perror("socket"); exit(1); }

	int opt = 1;
	if (setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt)) < 0) {
		perror("setsockopt"); exit(1);
	}

	struct sockaddr_in a = {AF_INET, htons(42069), {INADDR_ANY}};
	if (bind(s, (struct sockaddr*)&a, sizeof(a)) < 0) {
		perror("bind"); exit(1);
	}

	if (listen(s, 5) < 0) {
		perror("listen"); exit(1);
	}
	puts("First listen() succeeded");

	if (listen(s, 10) < 0) {
		perror("listen again"); exit(1);
	}
	puts("Second listen() succeeded");

	int c = accept(s, NULL, NULL);
	if (c < 0) { perror("accept"); exit(1); }
	puts("Connection accepted, starting echo server");

	char buf[1024];
	ssize_t n;
	while ((n = read(c, buf, sizeof(buf))) > 0) {
		if (write(c, buf, n) < 0) { perror("write"); exit(1); }
	}
	if (n < 0) { perror("read"); exit(1); }

	if (close(c) < 0) { perror("close c"); exit(1); }
	if (close(s) < 0) { perror("close s"); exit(1); }

	return 0;
}
