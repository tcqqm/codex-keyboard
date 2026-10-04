#include "speaker_assets/completion_phrase.h"

namespace easy_input::speaker_assets {
namespace {

#define DECLARE_COMPLETION_PHRASE(slot)                                   \
  extern const std::uint8_t kCompletion##slot##Start[]                    \
      asm("_binary_completion_" #slot "_eiad_start");                     \
  extern const std::uint8_t kCompletion##slot##End[]                      \
      asm("_binary_completion_" #slot "_eiad_end")

DECLARE_COMPLETION_PHRASE(1);
DECLARE_COMPLETION_PHRASE(2);
DECLARE_COMPLETION_PHRASE(3);
DECLARE_COMPLETION_PHRASE(4);

#undef DECLARE_COMPLETION_PHRASE

EmbeddedCompletionPhrase phrase(const std::uint8_t* begin,
                                const std::uint8_t* end) {
  return {begin, static_cast<std::size_t>(end - begin)};
}

}  // namespace

EmbeddedCompletionPhrase completion_phrase(std::uint8_t slot) {
  switch (slot) {
    case 1: return phrase(kCompletion1Start, kCompletion1End);
    case 2: return phrase(kCompletion2Start, kCompletion2End);
    case 3: return phrase(kCompletion3Start, kCompletion3End);
    case 4: return phrase(kCompletion4Start, kCompletion4End);
    default: return {};
  }
}

}  // namespace easy_input::speaker_assets
