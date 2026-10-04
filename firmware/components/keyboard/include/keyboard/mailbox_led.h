#pragma once

#include <array>
#include <cstdint>

#include "keyboard/input_feedback.h"

namespace easy_codex {

ai_keyboard::FeedbackColor mailbox_color_for_coverage(
    std::uint32_t coverage_count);

ai_keyboard::FeedbackColor task_activity_color(std::uint8_t running_tasks);

// 没连上 Wi-Fi 满 8 秒，或 Wi-Fi 已连上但信箱静默满 12 秒，才算本地链路故障。
constexpr std::uint32_t kWifiLinkFaultGraceMs = 8000U;
constexpr std::uint32_t kHostLinkFaultGraceMs = 12000U;
constexpr std::uint32_t kLinkFaultBlinkHalfMs = 500U;

bool local_link_fault(bool wifi_associated,
                      std::uint32_t now_ms,
                      std::uint32_t offline_since_ms,
                      std::uint32_t mailbox_since_ms);

bool link_fault_blink_lit(std::uint32_t now_ms);

// 故障时最右灯红/灭交替。没有故障时仍按运行数上色。
ai_keyboard::FeedbackColor rightmost_status_color(std::uint8_t running_tasks,
                                                   bool link_fault,
                                                   bool blink_lit);

std::array<ai_keyboard::FeedbackColor, 5> mailbox_frame_for_slots(
    const std::array<std::uint8_t, 4>& coverage_by_slot,
    std::uint8_t running_tasks);

}  // namespace easy_codex
