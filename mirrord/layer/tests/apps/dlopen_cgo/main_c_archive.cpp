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

using namespace std;

static atomic<bool> running{true};

void signal_handler(int signum) {
    cout << "\nReceived signal " << signum << ", shutting down...\n";
    running = false;
}

string get_exe_dir() {
    char exe_path[PATH_MAX];
    ssize_t len = readlink("/proc/self/exe", exe_path, sizeof(exe_path) - 1);
    exe_path[len] = '\0';
    return string(dirname(exe_path));
}

int main() {
    // Install signal handlers
    signal(SIGINT,  signal_handler);
    signal(SIGTERM, signal_handler);

    string exe_dir = get_exe_dir();

    string server_so_path  = exe_dir + "/server/libcpp_server.so";
    string fileops_so_path = exe_dir + "/fileops/libcpp_fileops.so";

    // Load C++ server library
    void* server_handle = dlopen(
        server_so_path.c_str(),
        RTLD_LAZY | RTLD_NODELETE
    );
    if (!server_handle) {
        cerr << "dlopen(server) error: " << dlerror() << "\n";
        return 1;
    }

    // Load C++ fileops library
    void* fileops_handle = dlopen(
        fileops_so_path.c_str(),
        RTLD_LAZY | RTLD_NODELETE
    );
    if (!fileops_handle) {
        cerr << "dlopen(fileops) error: " << dlerror() << "\n";
        return 1;
    }

    // Resolve server symbols
    typedef int  (*StartServerFn)(const char*, int);
    typedef void (*StopServerFn)();

    StartServerFn StartServer =
        (StartServerFn)dlsym(server_handle, "cppStartServerWrapper");
    if (!StartServer) {
        cerr << "dlsym(cppStartServerWrapper) error: " << dlerror() << "\n";
        return 1;
    }

    StopServerFn StopServer =
        (StopServerFn)dlsym(server_handle, "cppStopServerWrapper");
    if (!StopServer) {
        cerr << "dlsym(cppStopServerWrapper) error: " << dlerror() << "\n";
        return 1;
    }

    // Resolve fileops symbol
    typedef char* (*ReadFileToStringFn)(const char*);

    ReadFileToStringFn ReadFileToString =
        (ReadFileToStringFn)dlsym(fileops_handle, "cppReadFileToStringWrapper");
    if (!ReadFileToString) {
        cerr << "dlsym(cppReadFileToStringWrapper) error: " << dlerror() << "\n";
        return 1;
    }

    // Start Go-backed server
    int rc = StartServer("127.0.0.1", 23333);
    if (rc != 0) {
        cerr << "StartServer returned " << rc << "\n";
        return 1;
    }

    cout << "Server started. Press Ctrl-C to stop.\n";

    // Main loop
    while (running) {
        this_thread::sleep_for(chrono::seconds(1));

        char* file_content = ReadFileToString("/app/test.txt");
        if (!file_content) {
            cerr << "ReadFileToString returned null\n";
            continue;
        }

        cout << "\n--- test.txt (via Go → C++ → dlopen) ---\n";
        cout << file_content;
        cout << "\n--- EOF ---\n";

        free(file_content);
    }

    // Graceful shutdown
    StopServer();
    this_thread::sleep_for(chrono::milliseconds(200));

    // IMPORTANT:
    // Do NOT dlclose() Go-backed C++ libraries
    return 0;
}
