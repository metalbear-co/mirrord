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

#include "libgo_server.h"

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
 * This test app calls `dlopen()` to load a c-shared cgo library.
 * To compile the app:
 * 1. build the go c-shared library: `go build -buildmode=c-shared -o libgo_server.so server.go`.
 * 2. build the cpp app: `g++ main.cpp -o out.cpp_dlopen_cgo -ldl`.
**/
int main() {
    // Install signal handlers
    signal(SIGINT,  signal_handler);
    signal(SIGTERM, signal_handler);

    string exe_dir = get_exe_dir();
    string so_path = exe_dir + "/libgo_server.so";

    // Load library
    void* handle = dlopen(so_path.c_str(), RTLD_LAZY);
    if (!handle) {
        cerr << "dlopen error: " << dlerror() << "\n";
        return 1;
    }

    typedef int  (*StartServerFn)(char*, int);
    typedef void (*RunFileOpsFn)();
    typedef void (*StopServerFn)();

    StartServerFn StartServer = (StartServerFn)dlsym(handle, "StartServer");
    StopServerFn  StopServer  = (StopServerFn)dlsym(handle, "StopServer");

    if (!StartServer || !StopServer) {
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

    // Main thread waits until a signal is received
    while (running) {
        this_thread::sleep_for(std::chrono::milliseconds(200));
    }

    // Graceful shutdown
    StopServer();

    // Let the server clean up
    this_thread::sleep_for(std::chrono::milliseconds(200));

    // DO NOT call dlclose() for Go shared libraries
    return 0;
}
