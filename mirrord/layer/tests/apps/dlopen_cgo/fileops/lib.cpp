#include "fileops.h"

#include "libgo_fileops_c_archive.h"

extern "C" char* cppReadFileToStringWrapper(const char* path) {
    return ReadFileToString((char*)path);
}

