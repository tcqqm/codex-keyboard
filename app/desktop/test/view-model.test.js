import { describe, expect, it } from "vitest";
import {
  completionNotice,
  freshReplyIndexes,
  nextPlayFocus,
  presentDashboardFailure,
  presentProbe,
  presentSlotReplies,
  presentSlotStatus,
  sortedTasks,
} from "../ui/view-model.js";

describe("desktop Host health view", () => {
  it("renders the authoritative healthy snapshot", () => {
    expect(
      presentProbe({
        connection: "healthy",
        health: {
          v: 1,
          status: "ready",
          host_version: "0.1.0",
          pid: 321,
          started_at_unix_ms: 1,
          socket: "run/host.sock",
          database_schema: 1,
          recovered_jobs_on_start: 2,
        },
      }),
    ).toEqual({
      tone: "ready",
      title: "Host 正常运行",
      detail: "启动时恢复了 2 个任务",
      version: "0.1.0",
      pid: "321",
      socket: "run/host.sock · 0600",
      schema: "v1",
    });
  });

  it("keeps offline distinct from invalid protocol", () => {
    expect(
      presentProbe({ connection: "offline", reason: "unreachable" }).tone,
    ).toBe("offline");
    expect(
      presentProbe({ connection: "protocol_error", reason: "invalid" }).tone,
    ).toBe("error");
  });
});

describe("desktop four-slot dashboard", () => {
  it("clears stale provider details and distinguishes protocol failures", () => {
    expect(presentDashboardFailure("protocol_error")).toEqual({
      taskCount: "状态不可用",
      providerState: "Host 响应异常",
      asrModel: "--",
      ttsModel: "--",
      voice: "--",
    });
    expect(presentDashboardFailure("offline").providerState).toBe(
      "Host 不可达",
    );
  });

  it("shows queue and retained unread coverage without exposing task IDs", () => {
    expect(
      presentSlotStatus({
        slot: 2,
        task_id: "019fa972-5cfa-75e1-9008-0b17ade9a347",
        task_name: "Task A",
        project: "Project A",
        binding_generation: 4,
        pending_jobs: 2,
        unread_generation: 3,
        unread_coverage: 5,
      }),
    ).toBe("Codex · 队列 2 · 待听总结 5 次");
  });

  it("names a finished slot without a spoken summary", () => {
    expect(completionNotice(1)).toBe("任务一已完成");
    expect(completionNotice(2)).toBe("任务二已完成");
    expect(completionNotice(3)).toBe("任务三已完成");
    expect(completionNotice(4)).toBe("任务四已完成");
    expect(completionNotice(0)).toBe("任务已完成");
  });

  it("marks only replies that arrived after the last look", () => {
    expect(freshReplyIndexes(["甲", "已"], ["甲", "已"])).toEqual([]);
    expect(freshReplyIndexes(["甲", "已"], ["甲", "已", "丙"])).toEqual([2]);
    expect(freshReplyIndexes(["甲", "乙", "丙", "丁"], ["乙", "丙", "丁", "戊"])).toEqual([
      3,
    ]);
    expect(freshReplyIndexes(["甲"], ["甲改过"])).toEqual([0]);
    expect(freshReplyIndexes([], ["第一句"])).toEqual([0]);
    expect(freshReplyIndexes(["甲", "乙", "丙"], ["甲", "乙"])).toEqual([]);
  });

  it("jumps to the slot of a new play key and ignores a stale one", () => {
    const unseen = { ready: false, seq: 0, at: 0 };
    expect(nextPlayFocus(unseen, { slot: 2, seq: 4, at_unix_ms: 1_000 }, 20_000)).toEqual({
      ready: true,
      seq: 4,
      at: 1_000,
      slot: null,
    });
    expect(nextPlayFocus(unseen, { slot: 2, seq: 4, at_unix_ms: 19_000 }, 20_000)).toEqual({
      ready: true,
      seq: 4,
      at: 19_000,
      slot: 2,
    });
    const seen = { ready: true, seq: 4, at: 9_000 };
    expect(nextPlayFocus(seen, { slot: 1, seq: 5, at_unix_ms: 1 }, 9_000)).toEqual({
      ready: true,
      seq: 5,
      at: 1,
      slot: 1,
    });
    expect(nextPlayFocus(seen, { slot: 1, seq: 4, at_unix_ms: 9_000 }, 9_000)).toEqual({
      ready: true,
      seq: 4,
      at: 9_000,
      slot: null,
    });
    expect(nextPlayFocus(seen, { slot: 2, seq: 4, at_unix_ms: 12_000 }, 20_000)).toEqual({
      ready: true,
      seq: 4,
      at: 12_000,
      slot: 2,
    });
    expect(nextPlayFocus(seen, { slot: 6, seq: 8, at_unix_ms: 9_000 }, 9_000)).toEqual({
      ready: true,
      seq: 4,
      at: 9_000,
      slot: null,
    });
  });

  it("shows assistant replies and ignores blank lines", () => {
    expect(
      presentSlotReplies({
        slot: 1,
        task_id: "019fa972-5cfa-75e1-9008-0b17ade9a347",
        task_name: "Task A",
        project: "Project A",
        binding_generation: 4,
        pending_jobs: 0,
        unread_generation: null,
        unread_coverage: null,
        replies: ["甲", "  ", "已"],
      }),
    ).toEqual(["甲", "已"]);
  });

  it("orders pinned tasks before recent tasks without mutating the source", () => {
    const tasks = [
      {
        task_id: "b",
        name: "B",
        project: "P",
        updated_at_ms: 20,
        pinned: false,
      },
      {
        task_id: "a",
        name: "A",
        project: "P",
        updated_at_ms: 10,
        pinned: true,
      },
    ];
    expect(sortedTasks(tasks).map((task) => task.task_id)).toEqual(["a", "b"]);
    expect(tasks.map((task) => task.task_id)).toEqual(["b", "a"]);
  });
});
