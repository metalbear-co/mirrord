#include <stdio.h>
#include <string.h>
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

	int d = dup(s);
	if (d < 0) { perror("dup"); exit(1); }

	char buf[1024];
	char *resp = "HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nhello world\r\n";

	int c1 = accept(s, NULL, NULL);
	if (c1 < 0) { perror("accept"); exit(1); }

	if (read(c1, buf, sizeof(buf)) < 0) { perror("read"); exit(1); }
	if (write(c1, resp, strlen(resp)) < 0) { perror("write"); exit(1); }
	if (close(c1) < 0) { perror("close c1"); exit(1); }
	puts("Request 1 served on original socket");

	if (close(s) < 0) { perror("close s"); exit(1); }

	int c2 = accept(d, NULL, NULL);
	if (c2 < 0) { perror("accept"); exit(1); }

	if (read(c2, buf, sizeof(buf)) < 0) { perror("read"); exit(1); }
	if (write(c2, resp, strlen(resp)) < 0) { perror("write"); exit(1); }
	if (close(c2) < 0) { perror("close c2"); exit(1); }
	puts("Request 2 served on duplicated socket");

	if (close(d) < 0) { perror("close d"); exit(1); }

	return 0;
}
