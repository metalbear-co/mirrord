#include <dlfcn.h>
#include <iostream>
#include <thread>
#include <atomic>
#include <csignal>
#include <chrono>
#include <limits.h>
#include <unistd.h>
#include <string>
#include <libgen.h>
#include <cstdlib>

#include "server/libgo_server.h"
#include "fileops/libgo_fileops.h"

using namespace std;

static atomic<bool> running{true};

void signal_handler(int signum) {
    cout << "\nReceived signal " << signum << ", shutting down...\n";
    running = false;
}

string get_exe_dir() {
    char exe_path[PATH_MAX];
    ssize_t len = readlink("/proc/self/exe", exe_path, sizeof(exe_path)-1);
    exe_path[len] = '\0';
    return string(dirname(exe_path));
}

/**
 * This test app calls `dlopen()` to load two different c-shared cgo libraries.
 * To compile the app, run the script `./build_test_app.sh`, which does the following:
 * 1. build the go c-shared server library: `go build -buildmode=c-shared -o server/libgo_server.so server/main.go`.
 * 2. build the go c-shared file ops library: `go build -buildmode=c-shared -o fileops/libgo_fileops.so fileops/main.go`.
 * 3. build the cpp app: `g++ main.cpp -o out.cpp_dlopen_cgo -ldl`.
**/
int main() {
    // Install signal handlers
    signal(SIGINT,  signal_handler);
    signal(SIGTERM, signal_handler);

    string exe_dir = get_exe_dir();
    string server_so_path = exe_dir + "/server/libgo_server.so";
    string fileops_so_path = exe_dir + "/fileops/libgo_fileops.so";

	// These dlopen() flags are provided by the user.
    void* server_so_handle = dlopen(server_so_path.c_str(), RTLD_LAZY | RTLD_NODELETE | RTLD_DEEPBIND);
    if (!server_so_handle) {
        cerr << "dlopen error: " << dlerror() << "\n";
        return 1;
    }
    void* fileops_so_handle = dlopen(fileops_so_path.c_str(), RTLD_LAZY | RTLD_NODELETE | RTLD_DEEPBIND);
    if (!fileops_so_handle) {
        cerr << "dlopen error: " << dlerror() << "\n";
        return 1;
    }

	// Exported functions from the server library
    typedef int  (*StartServerFn)(char*, int);
    typedef void (*StopServerFn)();
    StartServerFn StartServer = (StartServerFn)dlsym(server_so_handle, "StartServer");
    if (!StartServer) {
        cerr << "dlsym error: " << dlerror() << "\n";
        return 1;
    }
    StopServerFn  StopServer  = (StopServerFn)dlsym(server_so_handle, "StopServer");
    if (!StopServer) {
        cerr << "dlsym error: " << dlerror() << "\n";
        return 1;
    }

	// Exported function from the file ops library
	typedef char* (*ReadFileToStringFn)(char*);
	ReadFileToStringFn ReadFileToString = (ReadFileToStringFn)dlsym(fileops_so_handle, "ReadFileToString");
    if (!ReadFileToString) {
        cerr << "dlsym error: " << dlerror() << "\n";
        return 1;
    }

    // Start Go server
    int rc = StartServer((char*)"127.0.0.1", 23333);
    if (rc != 0) {
        cerr << "StartServer returned " << rc << "\n";
        return 1;
    }

    cout << "Server started. Press Ctrl-C to stop.\n";

    // Main thread waits until a signal is received while reading 
	// the same file over and over.
    while (running) {
        this_thread::sleep_for(std::chrono::milliseconds(1000));
		char* file_content = ReadFileToString((char*)"/app/test.txt");
		if (!file_content) {
			cerr << "ReadFileToString returned null\n";
		} else {
			cout << "\n--- test.txt (via Go) ---\n";
			cout << file_content;
			cout << "\n--- EOF ---\n";
		}
		free(file_content);
    }

    // Graceful shutdown
    StopServer();

    // Let the server clean up
    this_thread::sleep_for(std::chrono::milliseconds(200));

    // DO NOT call dlclose() for Go shared libraries
    return 0;
}
