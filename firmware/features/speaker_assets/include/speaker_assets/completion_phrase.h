#pragma once

#include <cstddef>
#include <cstdint>

namespace easy_input::speaker_assets {

struct EmbeddedCompletionPhrase {
  const std::uint8_t* encoded = nullptr;
  std::size_t encoded_bytes = 0U;
};

// slot 为 1-4。其它值没有板载句子。
[[nodiscard]] EmbeddedCompletionPhrase completion_phrase(std::uint8_t slot);

}  // namespace easy_input::speaker_assets
