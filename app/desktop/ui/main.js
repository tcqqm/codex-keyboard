import {
  completionNotice,
  freshReplyIndexes,
  nextPlayFocus,
  presentDashboardFailure,
  presentProbe,
  presentSlotReplies,
  presentSlotStatus,
  sortedTasks,
} from "./view-model.js";

/** @param {string} selector @returns {HTMLElement} */
function requiredElement(selector) {
  const element = document.querySelector(selector);
  if (!(element instanceof HTMLElement)) {
    throw new Error(`missing UI element: ${selector}`);
  }
  return element;
}

const elements = {
  refresh: /** @type {HTMLButtonElement} */ (requiredElement("#refresh")),
  band: requiredElement("#status-band"),
  dot: requiredElement("#status-dot"),
  title: requiredElement("#status-title"),
  detail: requiredElement("#status-detail"),
  version: requiredElement("#host-version"),
  pid: requiredElement("#host-pid"),
  socket: requiredElement("#host-socket"),
  schema: requiredElement("#database-schema"),
  updated: requiredElement("#last-updated"),
  slots: requiredElement("#slots"),
  slotTemplate: /** @type {HTMLTemplateElement} */ (
    requiredElement("#slot-template")
  ),
  taskCount: requiredElement("#task-count"),
  providerDot: requiredElement("#provider-dot"),
  providerState: requiredElement("#provider-state"),
  asrModel: requiredElement("#asr-model"),
  ttsModel: requiredElement("#tts-model"),
  ttsVoice: requiredElement("#tts-voice"),
  slotView: requiredElement("#slot-view"),
  networkView: requiredElement("#network-view"),
  kicker: requiredElement("#detail-kicker"),
  detailTitle: requiredElement("#detail-title"),
  detailMeta: requiredElement("#detail-meta"),
  replies: requiredElement("#detail-replies"),
  detailTask: /** @type {HTMLSelectElement} */ (requiredElement("#detail-task")),
  detailBind: /** @type {HTMLButtonElement} */ (requiredElement("#detail-bind")),
  manage: /** @type {HTMLButtonElement} */ (requiredElement("#detail-manage")),
  bindBar: requiredElement("#bind-bar"),
  reading: requiredElement("#slot-reading"),
  configureLan: /** @type {HTMLButtonElement} */ (
    requiredElement("#configure-lan")
  ),
};

/** @type {import("./view-model.js").DashboardSnapshot | null} */
let dashboard = null;
const dirtySlots = new Set();
/** 刷新时保留还没点绑定的选择。 */
const draftTaskId = new Map();
let selectedSlot = 1;
let showingNetwork = false;
let manageOpen = false;
/** 每个槽上次看过的回复。打开窗口时已有的内容先记成看过。 */
const seenReplies = new Map();

/** @param {import("./view-model.js").HostView} view */
function renderHealth(view) {
  elements.band.className = `status-line ${view.tone}`;
  elements.dot.className = `status-dot ${view.tone}`;
  elements.title.textContent = view.title;
  elements.detail.textContent = view.detail;
  elements.version.textContent = view.version;
  elements.pid.textContent = view.pid;
  elements.socket.textContent = view.socket;
  elements.schema.textContent = view.schema;
  elements.updated.textContent = `更新于 ${new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date())}`;
}

/** @param {Element | null} element @returns {HTMLElement} */
function rowElement(element) {
  if (!(element instanceof HTMLElement))
    throw new Error("invalid slot template");
  return element;
}

/** @param {HTMLElement} row @param {string} selector @returns {HTMLElement} */
function childElement(row, selector) {
  const element = row.querySelector(selector);
  if (!(element instanceof HTMLElement))
    throw new Error("invalid slot template");
  return element;
}

/** @param {number} slot @returns {import("./view-model.js").DashboardSlot | undefined} */
function slotSnapshot(slot) {
  return dashboard?.slots.find((candidate) => candidate.slot === slot);
}

/** @param {number} slot @returns {HTMLElement | null} */
function threadButton(slot) {
  const button = elements.slots.querySelector(`[data-slot="${slot}"]`);
  return button instanceof HTMLElement ? button : null;
}

/** @param {import("./view-model.js").DashboardSlot | undefined} slot @returns {string} */
function assistantName(slot) {
  switch (slot?.assistant) {
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

/** @param {string | null | undefined} project @returns {string} */
function folderName(project) {
  if (!project) return "";
  return project.split("/").filter(Boolean).pop() ?? project;
}

/** @param {number} slot */
function revealSlot(slot) {
  const openingAnother = showingNetwork || selectedSlot !== slot;
  if (openingAnother && !showingNetwork) acknowledgeSlot(selectedSlot);
  selectedSlot = slot;
  showingNetwork = false;
  manageOpen = false;
  paintSelection();
  paintFreshMarks();
  renderDetail();
  threadButton(slot)?.scrollIntoView({ block: "nearest" });
  const invoke = window.__TAURI__?.core?.invoke;
  if (invoke) void invoke("focus_main_window");
}

function paintSelection() {
  for (const button of elements.slots.querySelectorAll(".thread")) {
    if (!(button instanceof HTMLElement)) continue;
    const active = !showingNetwork && Number(button.dataset.slot) === selectedSlot;
    button.classList.toggle("active", active);
    button.setAttribute("aria-current", active ? "true" : "false");
  }
  elements.configureLan.classList.toggle("active", showingNetwork);
  elements.configureLan.setAttribute("aria-pressed", String(showingNetwork));
  elements.slotView.hidden = showingNetwork;
  elements.networkView.hidden = !showingNetwork;
}

/** @param {number} slot */
function ensureThread(slot) {
  const existing = threadButton(slot);
  if (existing) return existing;
  const fragment = elements.slotTemplate.content.cloneNode(true);
  if (!(fragment instanceof DocumentFragment))
    throw new Error("invalid slot template");
  const button = rowElement(fragment.querySelector(".thread"));
  button.dataset.slot = String(slot);
  childElement(button, ".talk-key").textContent = `S${slot}`;
  childElement(button, ".slot-label").textContent = "未绑定";
  childElement(button, ".slot-meta").textContent = `按 S${slot} 说话`;
  button.addEventListener("click", () => {
    const openingAnother = showingNetwork || selectedSlot !== slot;
    if (openingAnother) {
      if (!showingNetwork) acknowledgeSlot(selectedSlot);
    } else {
      acknowledgeSlot(slot);
    }
    selectedSlot = slot;
    showingNetwork = false;
    manageOpen = false;
    paintSelection();
    paintFreshMarks();
    renderDetail();
  });
  elements.slots.append(button);
  return button;
}

/** @param {import("./view-model.js").DashboardTask[]} tasks @param {import("./view-model.js").DashboardSlot} slot */
function fillTaskSelect(tasks, slot) {
  const select = elements.detailTask;
  const selected = dirtySlots.has(slot.slot)
    ? (draftTaskId.get(slot.slot) ?? "")
    : (slot.task_id ?? "");
  const optionSignature = tasks
    .map((task) => `${task.task_id}\t${task.name}\t${task.project}\t${task.pinned}`)
    .join("|");
  if (select.dataset.options !== optionSignature || select.dataset.slot !== String(slot.slot)) {
    select.replaceChildren(new Option("选择任务", "", true, false));
    const placeholder = select.options.item(0);
    if (placeholder) placeholder.disabled = true;
    for (const task of tasks) {
      const marker = task.pinned ? "置顶 · " : "";
      select.add(
        new Option(`${marker}${task.name} · ${task.project}`, task.task_id),
      );
    }
    select.dataset.options = optionSignature;
    select.dataset.slot = String(slot.slot);
  }
  if (
    selected &&
    !Array.from(select.options).some((option) => option.value === selected)
  ) {
    const linked = slot.assistant && slot.assistant !== "codex";
    const label = linked
      ? `${slot.task_name ?? "新任务"} · ${folderName(slot.project)}`
      : `${slot.task_name ?? "不可用任务"} · 已移出列表`;
    select.add(new Option(label, selected));
  }
  select.value = selected;
  elements.detailBind.disabled = selected.length === 0;
}

function syncTaskControls(slot) {
  const bound = Boolean(slot?.task_id);
  const open = bound && manageOpen;
  // 绑定只在空槽，或点开「更换任务」后出现，不占正文。
  elements.bindBar.hidden = bound && !open;
  elements.manage.hidden = !bound;
  elements.manage.textContent = open ? "收起" : "更换任务";
  elements.manage.setAttribute("aria-expanded", String(open));
  elements.reading.classList.toggle("is-setup", !bound);
  elements.reading.classList.toggle("is-managing", open);
}

function renderDetail() {
  const slot = slotSnapshot(selectedSlot);
  elements.kicker.textContent = `S${selectedSlot} 说话 · S${selectedSlot + 4} 打开`;
  syncTaskControls(slot);
  if (!slot) {
    elements.detailTitle.textContent = "未绑定";
    elements.detailMeta.textContent = "正在读取这一槽";
    elements.replies.replaceChildren();
    elements.replies.dataset.signature = "";
    return;
  }
  const bound = Boolean(slot.task_id);
  elements.detailTitle.textContent = bound
    ? slot.task_name?.trim() || "未命名任务"
    : "未绑定";
  if (!bound) {
    elements.detailMeta.textContent = `从已有任务里选一个，或新建。按住 S${slot.slot} 说话。`;
  } else {
    const folder = folderName(slot.project);
    elements.detailMeta.textContent = folder
      ? `${presentSlotStatus(slot)} · ${folder}`
      : presentSlotStatus(slot);
  }
  renderReplies(slot);
  fillTaskSelect(sortedTasks(dashboard?.tasks ?? []), slot);
}

/**
 * 第一次见到这个槽，或换了绑定任务时，把当前回复记成已经看过。
 * @param {import("./view-model.js").DashboardSlot} slot
 * @returns {number[]}
 */
function slotFreshIndexes(slot) {
  const replies = presentSlotReplies(slot);
  const taskId = slot.task_id ?? "";
  const seen = seenReplies.get(slot.slot);
  if (!seen || seen.taskId !== taskId) {
    seenReplies.set(slot.slot, { taskId, replies: replies.slice() });
    return [];
  }
  return freshReplyIndexes(seen.replies, replies);
}

/** @param {number} slotNumber */
function acknowledgeSlot(slotNumber) {
  const slot = slotSnapshot(slotNumber);
  if (!slot) return;
  seenReplies.set(slotNumber, {
    taskId: slot.task_id ?? "",
    replies: presentSlotReplies(slot),
  });
}

function paintFreshMarks() {
  if (!dashboard) return;
  for (const slot of dashboard.slots) {
    const button = threadButton(slot.slot);
    if (!button) continue;
    const fresh = slotFreshIndexes(slot);
    button.classList.toggle("fresh", fresh.length > 0);
    const status = slot.task_id
      ? presentSlotStatus(slot)
      : `按住 S${slot.slot} 说话`;
    childElement(button, ".slot-meta").textContent =
      fresh.length > 0 ? completionNotice(slot.slot) : status;
  }
}

/** @param {HTMLElement} parent @param {number} slotNumber */
function appendFreshTag(parent, slotNumber) {
  const tag = document.createElement("span");
  tag.className = "fresh-tag";
  tag.textContent = completionNotice(slotNumber);
  parent.append(tag);
}

/** @param {import("./view-model.js").DashboardSlot} slot */
function renderReplies(slot) {
  const replies = presentSlotReplies(slot);
  const fresh = new Set(slotFreshIndexes(slot));
  const signature = `${slot.slot}\u0000${assistantName(slot)}\u0000${replies.join("\u0001")}\u0000${[...fresh].join(",")}`;
  if (elements.replies.dataset.signature === signature) return;
  elements.replies.dataset.signature = signature;
  elements.replies.replaceChildren();
  elements.replies.scrollTop = 0;
  if (!slot.task_id) return;
  if (replies.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty";
    empty.textContent = `这一槽还没有回复。按住 S${slot.slot} 说话。`;
    elements.replies.append(empty);
    return;
  }
  const newestFirst = replies.map((text, index) => ({ text, index })).reverse();
  const newest = newestFirst[0];
  const article = document.createElement("article");
  article.className = "message current";
  const who = document.createElement("span");
  who.className = "who";
  who.textContent = assistantName(slot);
  if (newest && fresh.has(newest.index)) {
    article.classList.add("is-fresh");
    appendFreshTag(who, slot.slot);
  }
  const paragraph = document.createElement("p");
  paragraph.textContent = newest?.text ?? "";
  article.append(who, paragraph);
  elements.replies.append(article);
  const older = newestFirst.slice(1);
  if (older.length === 0) return;
  const olderFresh = older.filter((item) => fresh.has(item.index)).length;
  const fold = document.createElement("details");
  fold.className = "earlier-fold";
  const summary = document.createElement("summary");
  const countLabel =
    older.length === 1 ? "更早的一条回复" : `更早的 ${older.length} 条回复`;
  summary.textContent = olderFresh > 0 ? `${countLabel} · 含已完成` : countLabel;
  fold.append(summary);
  for (const item of older) {
    const earlier = document.createElement("article");
    earlier.className = "message earlier";
    if (fresh.has(item.index)) {
      earlier.classList.add("is-fresh");
      appendFreshTag(earlier, slot.slot);
    }
    const body = document.createElement("p");
    body.textContent = item.text;
    earlier.append(body);
    fold.append(earlier);
  }
  elements.replies.append(fold);
}

/** @param {import("./view-model.js").DashboardSnapshot} snapshot */
function renderDashboard(snapshot) {
  dashboard = snapshot;
  const tasks = sortedTasks(snapshot.tasks);
  elements.taskCount.textContent = `${tasks.length} 个任务`;
  elements.providerDot.className = `provider-dot ${snapshot.provider.configured ? "ready" : "offline"}`;
  elements.providerState.textContent = snapshot.provider.configured
    ? "北京区 · 已就绪"
    : "北京区 · 未配置";
  elements.asrModel.textContent = snapshot.provider.asr_model;
  elements.ttsModel.textContent = snapshot.provider.tts_model;
  elements.ttsVoice.textContent = snapshot.provider.voice;

  for (const slot of [...snapshot.slots].sort((left, right) => left.slot - right.slot)) {
    const button = ensureThread(slot.slot);
    button.classList.toggle("unread", slot.unread_generation != null);
    childElement(button, ".slot-label").textContent = slot.task_name?.trim() || "未绑定";
  }
  paintFreshMarks();
  paintSelection();
  renderDetail();
}

const CREATE_ERRORS = {
  codex_missing: "找不到 Codex 命令",
  codex_failed: "新建任务失败",
  thread_missing: "没有拿到新任务",
  binding_rejected: "新任务还不能绑定",
  host_unreachable: "Host 未连接",
  home_unavailable: "找不到用户目录",
  invalid_name: "请填写 1 到 40 个字的名称",
  invalid_directory: "请选择一个文件夹",
  name_failed: "任务已创建，但名称没有写上",
};

/** @type {number | null} */
let createSlot = null;
/** @type {string} */
let createDirectory = "";

const createDialog = /** @type {HTMLDialogElement} */ (
  requiredElement("#create-dialog")
);
const createForm = /** @type {HTMLFormElement} */ (
  requiredElement("#create-form")
);
const createName = /** @type {HTMLInputElement} */ (
  requiredElement("#create-name")
);
const createProvider = /** @type {HTMLSelectElement} */ (
  requiredElement("#create-provider")
);
const createHint = requiredElement("#create-hint");
const PROVIDER_HINTS = {
  codex: "使用本机 Codex 登录。确认后马上建好，第一句话等你按键再说。",
  grok: "使用本机 Grok 登录。确认后可以直接说话，回复显示在这一行。",
  claude: "使用本机 Claude 登录。确认后可以直接说话，回复显示在这一行。",
  deepseek: "使用本机 DeepSeek 登录。确认后可以直接说话，回复显示在这一行。",
};
const createFolder = /** @type {HTMLButtonElement} */ (
  requiredElement("#create-folder")
);
const createError = requiredElement("#create-error");
const createConfirm = /** @type {HTMLButtonElement} */ (
  requiredElement("#create-confirm")
);

/** @param {number} slot */
function openCreateDialog(slot) {
  createSlot = slot;
  createDirectory = "";
  createName.value = "";
  createProvider.value = "codex";
  createHint.textContent = PROVIDER_HINTS.codex;
  createFolder.textContent = "选择文件夹";
  createFolder.title = "";
  createError.textContent = "";
  requiredElement("#create-title").textContent = `槽位 ${slot} 新建任务`;
  if (!createDialog.open) createDialog.showModal();
  createName.focus();
}

createProvider.addEventListener("change", () => {
  createHint.textContent =
    PROVIDER_HINTS[createProvider.value] ?? PROVIDER_HINTS.codex;
});

createFolder.addEventListener("click", async () => {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;
  try {
    const directory = await invoke("choose_directory");
    if (typeof directory !== "string" || directory.length === 0) return;
    createDirectory = directory;
    createFolder.textContent =
      directory.split("/").filter(Boolean).pop() ?? directory;
    createFolder.title = directory;
    createError.textContent = "";
  } catch (error) {
    if (error === "directory_cancelled") return;
    createError.textContent = "没能打开文件夹选择";
  }
});

requiredElement("#create-cancel").addEventListener("click", () => {
  createDialog.close();
});

createForm.addEventListener("submit", (event) => {
  event.preventDefault();
  void submitCreateDialog();
});

async function submitCreateDialog() {
  const slot = createSlot;
  if (slot === null) return;
  const name = createName.value.trim();
  if (name.length === 0) {
    createError.textContent = CREATE_ERRORS.invalid_name;
    return;
  }
  if (createDirectory.length === 0) {
    createError.textContent = CREATE_ERRORS.invalid_directory;
    return;
  }
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke || !dashboard) {
    createError.textContent = CREATE_ERRORS.host_unreachable;
    return;
  }
  const current = slotSnapshot(slot);
  createConfirm.disabled = true;
  createError.textContent = "正在创建…";
  try {
    const provider = createProvider.value;
    const updated = await invoke(
      provider === "codex" ? "create_and_bind_slot" : "bind_linked_assistant",
      {
        slot,
        provider,
        name,
        directory: createDirectory,
        expectedGeneration: current?.binding_generation ?? null,
      },
    );
    dirtySlots.delete(slot);
    draftTaskId.delete(slot);
    selectedSlot = slot;
    showingNetwork = false;
    manageOpen = false;
    renderDashboard(updated);
    createDialog.close();
  } catch (error) {
    const code = typeof error === "string" ? error : "";
    createError.textContent = CREATE_ERRORS[code] ?? "新建任务失败";
  } finally {
    createConfirm.disabled = false;
  }
}

elements.detailTask.addEventListener("change", () => {
  dirtySlots.add(selectedSlot);
  draftTaskId.set(selectedSlot, elements.detailTask.value);
  elements.detailBind.disabled = elements.detailTask.value.length === 0;
});

requiredElement("#detail-create").addEventListener("click", () => {
  openCreateDialog(selectedSlot);
});

elements.manage.addEventListener("click", () => {
  manageOpen = !manageOpen;
  renderDetail();
  if (manageOpen) elements.detailTask.focus();
});

document.addEventListener("click", (event) => {
  if (!manageOpen || createDialog.open) return;
  const target = event.target;
  if (!(target instanceof Node)) return;
  if (elements.bindBar.contains(target) || elements.manage.contains(target)) return;
  manageOpen = false;
  syncTaskControls(slotSnapshot(selectedSlot));
});

document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape" || !manageOpen || createDialog.open) return;
  manageOpen = false;
  syncTaskControls(slotSnapshot(selectedSlot));
  elements.manage.focus();
});

requiredElement("#bind-bar").addEventListener("submit", (event) => {
  event.preventDefault();
  void bindSlot(selectedSlot);
});

/** @param {number} slot */
async function bindSlot(slot) {
  const select = elements.detailTask;
  const button = elements.detailBind;
  const thread = threadButton(slot);
  if (!select.value || !dashboard) return;
  const current = slotSnapshot(slot);
  button.disabled = true;
  thread?.classList.add("saving");
  try {
    const invoke = window.__TAURI__?.core?.invoke;
    if (!invoke) throw new Error("tauri_unavailable");
    const updated = await invoke("bind_slot", {
      slot,
      taskId: select.value,
      expectedGeneration: current?.binding_generation ?? null,
    });
    dirtySlots.delete(slot);
    draftTaskId.delete(slot);
    manageOpen = false;
    renderDashboard(updated);
    thread?.classList.add("saved");
    window.setTimeout(() => thread?.classList.remove("saved"), 900);
  } catch {
    thread?.classList.add("failed");
    window.setTimeout(() => thread?.classList.remove("failed"), 1500);
    await refreshDashboard();
  } finally {
    button.disabled = elements.detailTask.value.length === 0;
    thread?.classList.remove("saving");
  }
}

async function refreshDashboard() {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) throw new Error("tauri_unavailable");
  const probe = await invoke("host_dashboard");
  if (probe.connection === "healthy") {
    renderDashboard(probe.dashboard);
    return;
  }
  const unavailable = presentDashboardFailure(probe.connection);
  elements.taskCount.textContent = unavailable.taskCount;
  elements.providerDot.className = "provider-dot offline";
  elements.providerState.textContent = unavailable.providerState;
  elements.asrModel.textContent = unavailable.asrModel;
  elements.ttsModel.textContent = unavailable.ttsModel;
  elements.ttsVoice.textContent = unavailable.voice;
}

async function refresh() {
  elements.refresh.disabled = true;
  elements.refresh.classList.add("spinning");
  try {
    const invoke = window.__TAURI__?.core?.invoke;
    if (!invoke) throw new Error("tauri_unavailable");
    const [probe] = await Promise.all([
      invoke("host_health"),
      refreshDashboard(),
    ]);
    renderHealth(presentProbe(probe));
    if (showingNetwork) await refreshWifiChoices();
  } catch {
    renderHealth(
      presentProbe({ connection: "offline", reason: "invoke_failed" }),
    );
  } finally {
    elements.refresh.disabled = false;
    elements.refresh.classList.remove("spinning");
  }
}

for (const slot of [1, 2, 3, 4]) ensureThread(slot);
paintSelection();
renderDetail();
elements.refresh.addEventListener("click", () => {
  void refresh();
});
void refresh();
window.setInterval(() => {
  void refresh();
}, 3000);

let playFocus = { ready: false, seq: 0, at: 0 };
let playFocusInflight = false;

async function pollPlayFocus() {
  if (playFocusInflight) return;
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;
  playFocusInflight = true;
  try {
    const cue = await invoke("host_play_focus");
    const next = nextPlayFocus(playFocus, cue, Date.now());
    const slot = next.slot;
    playFocus = { ready: next.ready, seq: next.seq, at: next.at };
    if (slot) revealSlot(slot);
  } catch {
    // 旧 Host 没有这个口时保持当前页。
  } finally {
    playFocusInflight = false;
  }
}

void pollPlayFocus();
window.setInterval(() => {
  void pollPlayFocus();
}, 300);

const lanForm = /** @type {HTMLFormElement} */ (requiredElement("#lan-form"));
const lanNetwork = /** @type {HTMLSelectElement} */ (
  requiredElement("#lan-network")
);
const lanCustom = requiredElement("#lan-custom-label");
const lanSsid = /** @type {HTMLInputElement} */ (requiredElement("#lan-ssid"));
const lanPasswordLabel = requiredElement("#lan-password-label");
const lanPassword = /** @type {HTMLInputElement} */ (
  requiredElement("#lan-password")
);
const lanMatch = requiredElement("#lan-match");
const lanHost = requiredElement("#lan-host");
const lanError = requiredElement("#lan-error");
const lanForget = /** @type {HTMLButtonElement} */ (requiredElement("#lan-forget"));
const lanReplace = /** @type {HTMLButtonElement} */ (requiredElement("#lan-replace"));
const lanSave = /** @type {HTMLButtonElement} */ (requiredElement("#lan-save"));
const lanConfirm = /** @type {HTMLButtonElement} */ (
  requiredElement("#lan-confirm")
);

const LAN_ERRORS = {
  wifi_unavailable: "名称无效。",
  lan_unavailable: "没读到这台 Mac 的局域网地址。",
  password_unavailable: "这个名称还没有匹配到密码。填一次，2.4 和 5G 会共用。",
  keyboard_unplugged: "没找到用数据线连着的键盘。蓝牙连着不算，请把数据线接到这台 Mac。",
  keyboard_permission:
    "系统仍不让这个窗口接收键盘回执。请到输入监控里把 Codex Keyboard.app 先关掉再打开，然后重新点一次写入。",
  keyboard_open_failed: "打不开键盘。确认数据线还插着，再试一次。",
  keyboard_write_failed: "网络送到一半断了。确认数据线还插着，再试一次。",
  keyboard_no_receipt: "网络送出去了，键盘没有回执。再点一次写入。",
  keyboard_rejected: "键盘没有收下这份网络。再点一次写入。",
  provision_failed: "写入没有完成。",
  home_unavailable: "读不到本机用户目录。",
  profile_unavailable: "保存的 Wi-Fi 读不出来。",
};

/** 写入或保存成功后的提示。刷新时不要把它盖掉。 */
let lanNotice = "";
/** 用户已经改过选择时，刷新不能把下拉框打回去。 */
let lanTouched = false;
/** 已匹配的网络要换一份新密码时，才重新显示密码框。 */
let lanReplacing = false;
const savedNames = new Set();

/** @param {unknown} error */
function lanErrorText(error) {
  if (typeof error === "string" && error in LAN_ERRORS) {
    return LAN_ERRORS[error];
  }
  return "没有完成。";
}

function choosingCustom() {
  return lanNetwork.selectedOptions[0]?.dataset.custom === "1";
}

function selectedSsid() {
  if (choosingCustom()) return lanSsid.value.trim();
  return lanNetwork.value.trim();
}

function bandNames(ssid) {
  const base = ssid.endsWith("-5G") && ssid.length > 3 ? ssid.slice(0, -3) : ssid;
  return [base, `${base}-5G`];
}

function selectedIsSaved() {
  const ssid = selectedSsid();
  return ssid.length > 0 && bandNames(ssid).some((name) => savedNames.has(name));
}

function syncLanForm() {
  lanCustom.hidden = !choosingCustom();
  const saved = selectedIsSaved();
  const replacing = saved && lanReplacing;
  lanForget.hidden = !saved;
  lanReplace.hidden = !saved || replacing;
  lanPasswordLabel.hidden = saved && !replacing;
  lanMatch.hidden = !saved || replacing;
  lanSave.hidden = saved && !replacing;
  if (saved && !replacing) lanPassword.value = "";
  lanPassword.placeholder = "Wi-Fi 密码";
}

/** @param {{ host?: string, networks?: { ssid: string, saved: boolean }[], suggested?: string } | null} choices */
function renderWifiChoices(choices) {
  const networks = Array.isArray(choices?.networks) ? choices.networks : [];
  const signature = networks
    .map((network) => `${network.ssid}\t${network.saved ? "1" : "0"}`)
    .join("\n");
  if (lanNetwork.dataset.signature !== signature) {
    const previous = selectedSsid();
    const keepCustom = lanTouched && choosingCustom();
    lanNetwork.replaceChildren();
    savedNames.clear();
    for (const network of networks) {
      if (typeof network.ssid !== "string" || network.ssid.length === 0) continue;
      const label = network.saved ? `${network.ssid} · 已保存` : network.ssid;
      const option = new Option(label, network.ssid);
      if (network.saved) {
        option.dataset.saved = "1";
        savedNames.add(network.ssid);
      }
      lanNetwork.add(option);
    }
    const custom = new Option("其他…", "@custom");
    custom.dataset.custom = "1";
    lanNetwork.add(custom);
    const values = Array.from(lanNetwork.options).map((option) => option.value);
    const suggested = typeof choices?.suggested === "string" ? choices.suggested : "";
    if (keepCustom) {
      lanNetwork.value = "@custom";
    } else if (lanTouched && previous && values.includes(previous)) {
      lanNetwork.value = previous;
    } else if (suggested && values.includes(suggested)) {
      lanNetwork.value = suggested;
    } else if (networks.length === 0) {
      lanNetwork.value = "@custom";
    } else {
      lanNetwork.selectedIndex = 0;
    }
    lanNetwork.dataset.signature = signature;
  } else {
    savedNames.clear();
    for (const network of networks) {
      if (network.saved && typeof network.ssid === "string") savedNames.add(network.ssid);
    }
  }
  syncLanForm();
  if (lanNotice) {
    lanHost.textContent = lanNotice;
    return;
  }
  if (typeof choices?.host === "string" && choices.host.length > 0) {
    lanHost.textContent = `键盘会来找 ${choices.host}:17333。`;
  }
}

async function refreshWifiChoices() {
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) {
    renderWifiChoices(null);
    return;
  }
  try {
    const choices = await invoke("wifi_choices");
    renderWifiChoices(choices);
  } catch (error) {
    if (lanNetwork.options.length === 0) renderWifiChoices(null);
    if (!lanNotice) lanError.textContent = lanErrorText(error);
  }
}

function showNetwork() {
  showingNetwork = true;
  paintSelection();
  if (lanNetwork.options.length === 0) {
    const waiting = new Option("正在读取网络…", "");
    waiting.disabled = true;
    lanNetwork.add(waiting);
  }
  void refreshWifiChoices();
}

async function saveLanProfile() {
  const ssid = selectedSsid();
  const password = lanPassword.value;
  if (ssid.length === 0) {
    lanError.textContent = "先选择网络。列表里没有的话，选其他再填名称。";
    return;
  }
  if (password.length === 0) {
    lanError.textContent = "填写 Wi-Fi 密码后再保存。2.4 和带 -5G 的名称会共用这一份。";
    return;
  }
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) {
    lanError.textContent = "应用还没准备好。";
    return;
  }
  lanSave.disabled = true;
  lanError.textContent = "";
  lanNotice = "";
  try {
    await invoke("save_wifi_profile", { ssid, password });
    lanPassword.value = "";
    lanTouched = true;
    lanReplacing = false;
    lanNotice = "已经记住这组 Wi-Fi。2.4 和带 -5G 的名称都会匹配这一份。";
    lanHost.textContent = lanNotice;
    await refreshWifiChoices();
  } catch (error) {
    lanError.textContent = lanErrorText(error);
  } finally {
    lanSave.disabled = false;
  }
}

async function forgetLanProfile() {
  const ssid = selectedSsid();
  if (!selectedIsSaved()) return;
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) return;
  lanForget.disabled = true;
  lanError.textContent = "";
  lanNotice = "";
  try {
    await invoke("forget_wifi_profile", { ssid });
    lanPassword.value = "";
    lanTouched = true;
    lanReplacing = false;
    lanNotice = "已经不再记住这组 Wi-Fi。";
    lanHost.textContent = lanNotice;
    await refreshWifiChoices();
  } catch (error) {
    lanError.textContent = lanErrorText(error);
  } finally {
    lanForget.disabled = false;
  }
}

async function submitLanForm() {
  const ssid = selectedSsid();
  const password = lanPassword.value;
  if (ssid.length === 0) {
    lanError.textContent = "先选择网络。列表里没有的话，选其他再填名称。";
    return;
  }
  if (password.length === 0 && !selectedIsSaved()) {
    lanError.textContent = "这个名称还没有匹配到密码。填一次，2.4 和 5G 会共用。";
    return;
  }
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) {
    lanError.textContent = "应用还没准备好。";
    return;
  }
  lanConfirm.disabled = true;
  lanError.textContent = "";
  lanNotice = "正在写入键盘…";
  lanHost.textContent = lanNotice;
  try {
    await invoke("provision_manual_network", { ssid, password });
  } catch (error) {
    lanNotice = "";
    lanError.textContent = lanErrorText(error);
    lanConfirm.disabled = false;
    return;
  }
  lanPassword.value = "";
  lanTouched = true;
  lanReplacing = false;
  lanNotice = "已经写进键盘。等最右边的灯亮，就是连上了。";
  if (password.length > 0) {
    try {
      await invoke("save_wifi_profile", { ssid, password });
    } catch {
      lanNotice = "已经写进键盘。这组密码没有记住。";
    }
  }
  lanHost.textContent = lanNotice;
  try {
    await refreshWifiChoices();
  } finally {
    lanConfirm.disabled = false;
  }
}

elements.configureLan.addEventListener("click", () => {
  showNetwork();
});
lanNetwork.addEventListener("change", () => {
  lanTouched = true;
  lanReplacing = false;
  lanPassword.value = "";
  lanNotice = "";
  lanError.textContent = "";
  syncLanForm();
});
lanSsid.addEventListener("input", () => {
  lanTouched = true;
  lanNotice = "";
  lanError.textContent = "";
  syncLanForm();
});
lanPassword.addEventListener("input", () => {
  lanNotice = "";
  lanError.textContent = "";
});
lanSave.addEventListener("click", () => {
  void saveLanProfile();
});
lanForget.addEventListener("click", () => {
  void forgetLanProfile();
});
lanReplace.addEventListener("click", () => {
  lanReplacing = true;
  lanNotice = "";
  lanError.textContent = "";
  syncLanForm();
  lanPassword.focus();
});
lanForm.addEventListener("submit", (event) => {
  event.preventDefault();
  void submitLanForm();
});
syncLanForm();
