#include "FrameBuffers.h"

namespace glue {

TerminalStatus FrameBuffers::copy(TerminalSession* session) {
    if (session == nullptr) {
        return TerminalStatus_NullHandle;
    }

    TerminalFrameBuffers buffers;
    buffers.runs = runs_.data();
    buffers.runs_cap = static_cast<std::uint32_t>(runs_.size());
    buffers.text = text_.data();
    buffers.text_cap = static_cast<std::uint32_t>(text_.size());

    TerminalFrameInfo info{};
    TerminalStatus status = terminal_copy_frame(session, &buffers, &info);
    if (status == TerminalStatus_BufferTooSmall) {
        // The failed call still reported the sizes it wanted, so the second one
        // always fits. Growth is one-way: a window that was once large will not
        // give the memory back, and in exchange no redraw after the first
        // allocates.
        if (info.runs_len > runs_.size()) {
            runs_.resize(info.runs_len);
        }
        if (info.text_len > text_.size()) {
            text_.resize(info.text_len);
        }
        buffers.runs = runs_.data();
        buffers.runs_cap = static_cast<std::uint32_t>(runs_.size());
        buffers.text = text_.data();
        buffers.text_cap = static_cast<std::uint32_t>(text_.size());
        status = terminal_copy_frame(session, &buffers, &info);
    }

    if (status == TerminalStatus_Ok) {
        info_ = info;
    }
    return status;
}

std::string_view FrameBuffers::run_text(const TerminalRun& run) const {
    if (run.utf8_offset + run.utf8_len > info_.text_len) {
        return {};  // never trust an offset far enough to read past the buffer
    }
    return {reinterpret_cast<const char*>(text_.data()) + run.utf8_offset, run.utf8_len};
}

}  // namespace glue
