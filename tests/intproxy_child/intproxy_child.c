#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <errno.h>

// This is for verifying that mirrod-intproxy does not remain a child
// of the user process
int main() {
	int rv = wait(0);
	if (rv == -1 && errno == ECHILD) {
		return 0;
	} else {
		printf("ERROR: wait(2) returned %d\n", rv);
		printf("errno = %d (%s)\n", errno, strerror(errno));
		return 1;
	}
}
