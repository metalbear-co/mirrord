#pragma once

#ifdef __cplusplus
extern "C" {
#endif

int  cppStartServerWrapper(const char* host, int port);
void cppStopServerWrapper();

#ifdef __cplusplus
}
#endif

