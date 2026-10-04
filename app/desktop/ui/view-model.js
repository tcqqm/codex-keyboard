/**
 * @typedef {{
 *   v: number,
 *   status: "ready",
 *   host_version: string,
 *   pid: number,
 *   started_at_unix_ms: number,
 *   socket: string,
 *   database_schema: number,
 *   recovered_jobs_on_start: number
 * }} HealthSnapshot
 *
 * @typedef {
 *   | {connection: "healthy", health: HealthSnapshot}
 *   | {connection: "offline", reason: string}
 *   | {connection: "protocol_error", reason: string}
 * } HostProbe
 *
 * @typedef {{
 *   tone: "ready" | "offline" | "error",
 *   title: string,
 *   detail: string,
 *   version: string,
 *   pid: string,
 *   socket: string,
 *   schema: string
 * }} HostView
 *
 * @typedef {{
 *   task_id: string,
 *   name: string,
 *   project: string,
 *   updated_at_ms: number,
 *   pinned: boolean
 * }} DashboardTask
 *
 * @typedef {{
 *   slot: number,
 *   task_id: string | null,
 *   task_name: string | null,
 *   project: string | null,
 *   binding_generation: number | null,
 *   pending_jobs: number,
 *   unread_generation: number | null,
 *   unread_coverage: number | null,
 *   replies?: string[],
 *   assistant?: string
 * }} DashboardSlot
 *
 * @typedef {{
 *   v: number,
 *   tasks: DashboardTask[],
 *   slots: DashboardSlot[],
 *   provider: {
 *     configured: boolean,
 *     region: string,
 *     asr_model: string,
 *     tts_model: string,
 *     voice: string
 *   }
 * }} DashboardSnapshot
 */

/** @param {HostProbe} probe @returns {HostView} */
export function presentProbe(probe) {
  if (probe.connection === "healthy") {
    const recovered = probe.health.recovered_jobs_on_start;
    return {
      tone: "ready",
      title: "Host 正常运行",
      detail:
        recovered > 0 ? `启动时恢复了 ${recovered} 个任务` : "后台监控已就绪",
      version: probe.health.host_version,
      pid: String(probe.health.pid),
      socket: `${probe.health.socket} · 0600`,
      schema: `v${probe.health.database_schema}`,
    };
  }
  if (probe.connection === "offline") {
    return {
      tone: "offline",
      title: "Host 未连接",
      detail: "常驻服务当前不可达",
      version: "--",
      pid: "--",
      socket: "run/host.sock · 离线",
      schema: "--",
    };
  }
  return {
    tone: "error",
    title: "Health 协议异常",
    detail: "Host 响应未通过本地协议校验",
    version: "--",
    pid: "--",
    socket: "run/host.sock · 拒绝",
    schema: "--",
  };
}

/** @param {string | undefined} provider @returns {string} */
function assistantLabel(provider) {
  switch (provider) {
    case "grok":
      return "Grok";
    case "claude":
      return "Claude";
    case "deepseek":
      return "DeepSeek";
    default:
      return "Codex";
  }
}

/** @param {DashboardSlot} slot @returns {string} */
export function presentSlotStatus(slot) {
  if (!slot.task_id) return "未绑定";
  const facts = [assistantLabel(slot.assistant)];
  if (slot.pending_jobs > 0) {
    facts.push(`队列 ${slot.pending_jobs}`);
  }
  if (slot.unread_generation !== null) {
    const coverage = slot.unread_coverage ?? 1;
    facts.push(`待听总结 ${coverage} 次`);
  }
  if (facts.length === 1) facts.push("已绑定");
  return facts.join(" · ");
}

const COMPLETION_NOTICES = ["任务一已完成", "任务二已完成", "任务三已完成", "任务四已完成"];

/** @param {number} slot @returns {string} */
export function completionNotice(slot) {
  return COMPLETION_NOTICES[slot - 1] ?? "任务已完成";
}

/**
 * 下排按键把窗口带到对应槽。第一次看到很久以前的按键不跳；App 刚被拉起来时，几秒内的按键要跳。
 * Host 重启后序号会从 1 再计，所以同一序号但按下时间变了，也要打开那一槽。
 * @param {{ ready: boolean, seq: number, at?: number }} seen
 * @param {{ slot?: number, seq?: number, at_unix_ms?: number } | null | undefined} cue
 * @param {number} nowMs
 * @returns {{ ready: boolean, seq: number, at: number, slot: number | null }}
 */
export function nextPlayFocus(seen, cue, nowMs) {
  const seq = Number(cue?.seq);
  const slot = Number(cue?.slot);
  const at = Number(cue?.at_unix_ms);
  const seenAt = seen.at ?? 0;
  const validSlot = Number.isInteger(slot) && slot >= 1 && slot <= 4;
  if (!Number.isInteger(seq) || seq <= 0 || !Number.isInteger(at)) {
    return { ready: seen.ready, seq: seen.seq, at: seenAt, slot: null };
  }
  if (!seen.ready) {
    const fresh = validSlot && nowMs >= at && nowMs - at <= 4000;
    return { ready: true, seq, at, slot: fresh ? slot : null };
  }
  if ((seq === seen.seq && at === seenAt) || !validSlot) {
    return { ready: true, seq: seen.seq, at: seenAt, slot: null };
  }
  return { ready: true, seq, at, slot };
}

/** @param {DashboardSlot} slot @returns {string[]} */
export function presentSlotReplies(slot) {
  return (slot.replies ?? []).filter((reply) => reply.trim().length > 0);
}

/**
 * 仪表盘只留最近几条，更早的会从前面滑出去，最后一条也可能被改写。
 * 在各种前后对齐里取最长的已看过前缀，后面的下标才是新回复。
 * @param {string[]} seen 上次看过的回复，从早到晚
 * @param {string[]} current 当前回复，从早到晚
 * @returns {number[]}
 */
export function freshReplyIndexes(seen, current) {
  let matchedPrefix = -1;
  for (let shift = 0; shift <= seen.length; shift += 1) {
    const limit = Math.min(seen.length - shift, current.length);
    let matched = 0;
    while (matched < limit && seen[shift + matched] === current[matched]) {
      matched += 1;
    }
    if (matched > matchedPrefix) matchedPrefix = matched;
  }
  const freshStart = Math.max(matchedPrefix, 0);
  const indexes = [];
  for (let index = freshStart; index < current.length; index += 1) {
    indexes.push(index);
  }
  return indexes;
}

/** @param {DashboardTask[]} tasks @returns {DashboardTask[]} */
export function sortedTasks(tasks) {
  return [...tasks].sort(
    (left, right) =>
      Number(right.pinned) - Number(left.pinned) ||
      right.updated_at_ms - left.updated_at_ms ||
      left.name.localeCompare(right.name, "zh-CN"),
  );
}

/** @param {"offline" | "protocol_error"} connection */
export function presentDashboardFailure(connection) {
  const protocolError = connection === "protocol_error";
  return {
    taskCount: protocolError ? "状态不可用" : "Host 离线",
    providerState: protocolError ? "Host 响应异常" : "Host 不可达",
    asrModel: "--",
    ttsModel: "--",
    voice: "--",
  };
}
