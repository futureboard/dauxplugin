/*
 * Abi.cpp - ABI version helpers and result-code formatting.
 */
#include <DAUx/Core.hpp>

#include <cstdio>

namespace daux {

const char* result_to_string(daux_result r) noexcept {
    switch (r) {
        case DAUX_OK:                   return "OK";
        case DAUX_ERR_UNKNOWN:          return "unknown error";
        case DAUX_ERR_INVALID_ARG:      return "invalid argument";
        case DAUX_ERR_NOT_SUPPORTED:    return "not supported";
        case DAUX_ERR_NOT_INITIALIZED:  return "not initialized";
        case DAUX_ERR_OUT_OF_MEMORY:    return "out of memory";
        case DAUX_ERR_INVALID_STATE:    return "invalid state";
        case DAUX_ERR_BUFFER_TOO_SMALL: return "buffer too small";
        case DAUX_ERR_ABI_MISMATCH:     return "ABI mismatch";
        case DAUX_ERR_NOT_FOUND:        return "not found";
        case DAUX_ERR_IO:               return "I/O error";
        case DAUX_ERROR_PLUGIN_FAILED:  return "plugin failed";
        case DAUX_ERROR_HOST_FAILED:    return "host failed";
        default:                        return "unrecognized result code";
    }
}

bool abi_compatible(uint32_t plugin_abi_version) noexcept {
    return DAUX_VERSION_MAJOR(plugin_abi_version) == DAUX_ABI_VERSION_MAJOR;
}

std::string version_to_string(uint32_t packed) {
    char buf[32];
    std::snprintf(buf, sizeof(buf), "%u.%u.%u",
                  (unsigned)DAUX_VERSION_MAJOR(packed),
                  (unsigned)DAUX_VERSION_MINOR(packed),
                  (unsigned)DAUX_VERSION_PATCH(packed));
    return std::string(buf);
}

} // namespace daux
