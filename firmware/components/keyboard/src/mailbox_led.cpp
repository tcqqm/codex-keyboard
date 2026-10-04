#include "keyboard/mailbox_led.h"

#include <algorithm>

namespace easy_codex {

ai_keyboard::FeedbackColor mailbox_color_for_coverage(
    std::uint32_t coverage_count) {
  if (coverage_count == 0U) {
    return {};
  }
  const auto bounded = std::min<std::uint32_t>(coverage_count, 16U);
  // 1 次未听和最右空闲绿灯一样亮（绿 24、蓝 5），之后每多一次更亮，16 次饱和。
  const auto steps = bounded - 1U;
  const auto green = static_cast<std::uint8_t>(24U + steps * 4U);
  const auto blue = static_cast<std::uint8_t>(5U + steps / 3U);
  return {0U, green, blue};
}

bool local_link_fault(bool wifi_associated,
                      std::uint32_t now_ms,
                      std::uint32_t offline_since_ms,
                      std::uint32_t mailbox_since_ms) {
  if (!wifi_associated) {
    return now_ms - offline_since_ms >= kWifiLinkFaultGraceMs;
  }
  return now_ms - mailbox_since_ms >= kHostLinkFaultGraceMs;
}

bool link_fault_blink_lit(std::uint32_t now_ms) {
  return (now_ms / kLinkFaultBlinkHalfMs) % 2U == 0U;
}

ai_keyboard::FeedbackColor rightmost_status_color(std::uint8_t running_tasks,
                                                   bool link_fault,
                                                   bool blink_lit) {
  if (link_fault) {
    // 和四个任务的常红同色，用亮灭区分故障。
    return blink_lit ? ai_keyboard::FeedbackColor{48U, 0U, 0U}
                     : ai_keyboard::FeedbackColor{};
  }
  return task_activity_color(running_tasks);
}

ai_keyboard::FeedbackColor task_activity_color(std::uint8_t running_tasks) {
  switch (std::min<std::uint8_t>(running_tasks, 4U)) {
    case 0U:
      return {0U, 24U, 5U};
    case 1U:
      return {38U, 30U, 0U};
    case 2U:
      return {46U, 16U, 0U};
    case 3U:
      return {26U, 0U, 40U};
    default:
      return {48U, 0U, 0U};
  }
}

std::array<ai_keyboard::FeedbackColor, 5> mailbox_frame_for_slots(
    const std::array<std::uint8_t, 4>& coverage_by_slot,
    std::uint8_t running_tasks) {
  std::array<ai_keyboard::FeedbackColor, 5> frame{};
  // D1/frame 0 is the physical rightmost LED; D5/frame 4 is leftmost.
  frame[0U] = task_activity_color(running_tasks);
  for (std::size_t index = 0U; index < coverage_by_slot.size(); ++index) {
    frame[4U - index] = mailbox_color_for_coverage(coverage_by_slot[index]);
  }
  return frame;
}

}  // namespace easy_codex
