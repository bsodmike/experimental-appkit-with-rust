// The frame copy-out: caller-owned buffers, grown once and reused.
//
// PRD §10-A. The engine copies into buffers the frontend owns, and the two-call
// protocol — ask for the sizes, then ask again with room — is what makes that
// work without either side allocating in the steady state. Getting the regrow
// wrong is a silently truncated screen, so it lives here where it is tested.

#pragma once

#include <cstddef>
#include <cstdint>
#include <string_view>
#include <vector>

#include "terminal.h"

namespace glue {

class FrameBuffers {
  public:
    /// Copy the visible screen, growing the buffers if the frame outgrew them.
    ///
    /// Returns the status of the copy. Anything but `TerminalStatus_Ok` means
    /// the buffers hold the previous frame, not this one, and the caller should
    /// draw nothing rather than draw a mixture.
    TerminalStatus copy(TerminalSession* session);

    const TerminalFrameInfo& info() const { return info_; }
    const TerminalRun* runs() const { return runs_.data(); }
    std::size_t run_count() const { return info_.runs_len; }
    std::string_view text() const {
        return {reinterpret_cast<const char*>(text_.data()), info_.text_len};
    }

    /// The text of one run, as a view into the frame's buffer.
    std::string_view run_text(const TerminalRun& run) const;

    /// How much the buffers currently hold, for tests and for anyone wondering
    /// whether a redraw allocates.
    std::size_t run_capacity() const { return runs_.size(); }
    std::size_t text_capacity() const { return text_.size(); }

  private:
    std::vector<TerminalRun> runs_;
    std::vector<std::uint8_t> text_;
    TerminalFrameInfo info_{};
};

}  // namespace glue
