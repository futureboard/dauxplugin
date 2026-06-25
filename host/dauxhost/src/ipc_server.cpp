/*
 * ipc_server.cpp - IPC transport abstraction (skeleton).
 *
 * DAUxHost talks to Futureboard over IPC so a plugin crash is isolated from the
 * DAW. This file defines the transport seam. The reference build provides a
 * no-op implementation; the intended production transport on Windows is a
 * duplex named pipe for control messages plus a shared-memory ring for audio.
 * See dauxhost-protocol.md for the wire format.
 */
#include "host.hpp"

#include <cstdio>
#include <utility>

namespace dauxhost {

IpcServer::IpcServer(std::string endpoint) : endpoint_(std::move(endpoint)) {}

IpcServer::~IpcServer() { stop(); }

bool IpcServer::start() {
    if (endpoint_.empty()) {
        // No endpoint requested -> transport intentionally inactive.
        return false;
    }
    /* TODO: create the named pipe / shared-memory segment named `endpoint_`.
     * For now we report "unavailable" so callers fall back to idle behavior. */
    running_ = false;
    return running_;
}

void IpcServer::stop() {
    if (running_) {
        // TODO: close handles, unmap shared memory.
        running_ = false;
    }
}

bool IpcServer::send(const std::string& /*msg*/) {
    if (!running_) return false;
    // TODO: write a framed message to the pipe.
    return false;
}

bool IpcServer::receive(std::string& /*out*/) {
    if (!running_) return false;
    // TODO: block until a framed message arrives; false on shutdown.
    return false;
}

} // namespace dauxhost
