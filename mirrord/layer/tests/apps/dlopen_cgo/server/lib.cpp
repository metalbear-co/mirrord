#include "server.h"

#include "libgo_server_c_archive.h"

extern "C" int cppStartServerWrapper(const char* host, int port) {
    return StartServer((char*)host, port);
}

extern "C" void cppStopServerWrapper() {
    StopServer();
}

