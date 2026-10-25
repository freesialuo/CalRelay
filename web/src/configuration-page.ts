import { AdminFailure } from "./admin-session";
import {
  type ConfigurationAPI,
  type Configurations,
  type Kind,
  labels,
  type Records,
  recordId,
  retired,
  revision,
  summaryId,
  summaryRetired,
  summaryRevision,
} from "./configuration-api";
import { configurationForm } from "./configuration-form";
import { node } from "./source-form";
import { SourcesAPI, validId } from "./sources-api";

export function mountConfiguration<K extends Kind>(
  root: HTMLElement,
  api: ConfigurationAPI<K>,
): { canLeave: () => boolean; dispose: () => void } {
  const label = labels[api.kind];
  const noun =
    api.kind === "outputs" ? "输出" : api.kind === "rules" ? "规则" : "订阅";
  let disposed = false;
  let dirty = false;
  let writing = false;
  let epoch = 0;
  let listEpoch = 0;
  let reading = false;
  let currentWrite: AbortController | undefined;
  let formAbort: AbortController | undefined;
  const requests = new Set<AbortController>();
  const writes = new Set<HTMLButtonElement>();
  let clearCapability = () => {};
  let capabilityPending = false;
  const status = node("p", "正在读取配置…");
  status.setAttribute("role", "status");
  const auth = node("p");
  auth.className = "notice";
  const refresh = node("button", `重新读取${noun}列表`);
  refresh.type = "button";
  const create = node("button", `创建${noun}`);
  create.type = "button";
  writes.add(create);
  const list = node("div");
  const editor = node("section");
  editor.className = "panel";
  root.append(
    node("h1", label),
    node(
      "p",
      "配置提交不等于工作器已执行。写入使用服务端版本条件，不自动覆盖或重复提交。",
    ),
    auth,
    status,
    refresh,
    create,
    list,
    editor,
  );
  const display = (message: string, state = "error") => {
    if (!disposed) {
      status.textContent = message;
      status.dataset.state = state;
    }
  };
  const fail = (e: unknown) =>
    display(
      e instanceof AdminFailure
        ? e.message
        : "未取得可信结果，请重新读取核实。",
    );
  const authorize = () => {
    auth.textContent = api.session.writable
      ? "当前浏览器持有短期管理写入授权。"
      : "当前没有可用管理写入授权，请验证管理身份；只读配置不提供密钥。";
    for (const b of writes)
      b.disabled =
        !api.session.writable ||
        writing ||
        reading ||
        b.dataset.blocked === "true";
  };
  const canLeave = () =>
    !(dirty || writing || capabilityPending) ||
    window.confirm(
      writing
        ? "请求可能已提交；离开会取消等待，结果未知。确认离开？"
        : capabilityPending
          ? "一次性链接尚在本页；离开后无法再次读取。确认已妥善保存或放弃？"
          : "存在未保存输入，确认放弃？",
    );
  let previouslyAuthorized = api.session.writable;
  const sessionChanged = () => {
    authorize();
    if (!api.session.busy) {
      const authorized = api.session.writable;
      if (!authorized) clearCapability();
      if (authorized && !previouslyAuthorized) void load();
      previouslyAuthorized = authorized;
    }
  };
  api.session.addEventListener("change", sessionChanged);
  const load = async () => {
    const generation = ++listEpoch;
    const controller = new AbortController();
    requests.add(controller);
    refresh.disabled = true;
    try {
      list.replaceChildren();
      let cursor: string | undefined;
      const pageOnce = async () => {
        const page = await api.list(cursor, controller.signal);
        if (disposed || controller.signal.aborted || generation !== listEpoch)
          return;
        for (const s of page.items) {
          const row = node("section");
          row.className = "panel";
          row.append(
            node("h2", s.name),
            node(
              "p",
              `ID：${summaryId(s)} · 版本 ${summaryRevision(s)} · ${summaryRetired(s) !== null ? "永久退休" : s.enabled ? "启用" : "禁用"}`,
            ),
          );
          if ("expires_at" in s && s.expires_at !== null)
            row.append(
              node(
                "p",
                s.expires_at <= Date.now()
                  ? "链接已到期（不公开密钥）"
                  : "链接有到期限制（不公开密钥）",
              ),
            );
          const open = node("button", "查看 / 编辑");
          open.type = "button";
          open.onclick = () => {
            if (writing || reading || !canLeave()) return;
            void select(summaryId(s));
          };
          row.append(open);
          list.append(row);
        }
        if (page.items.length === 0 && !cursor)
          list.append(
            node("p", "尚无托管配置；读取成功不代表系统已执行任何任务。"),
          );
        if (status.dataset.state === undefined)
          display("配置列表已读取；密钥不由列表提供。", "success");
        cursor = page.next_after ?? undefined;
        if (cursor) {
          const more = node("button", "读取更多配置");
          more.type = "button";
          more.onclick = () => {
            more.disabled = true;
            void pageOnce()
              .then(() => more.remove())
              .catch((e) => {
                if (!disposed && generation === listEpoch) {
                  fail(e);
                  more.disabled = false;
                }
              });
          };
          list.append(more);
        }
      };
      await pageOnce();
    } catch (e) {
      if (!disposed && generation === listEpoch) fail(e);
    } finally {
      requests.delete(controller);
      if (generation === listEpoch) refresh.disabled = false;
    }
  };
  const select = async (id: string) => {
    const generation = ++epoch;
    display(`正在读取${noun}详情…`, "pending");
    reading = true;
    editor.inert = true;
    editor.setAttribute("aria-busy", "true");
    authorize();
    const controls = Array.from(
      editor.querySelectorAll<
        | HTMLInputElement
        | HTMLButtonElement
        | HTMLFieldSetElement
        | HTMLSelectElement
      >("input,button,fieldset,select"),
    ).map((c) => [c, c.disabled] as const);
    for (const [c] of controls) c.disabled = true;
    const controller = new AbortController();
    requests.add(controller);
    try {
      const record = await api.read(id, controller.signal);
      if (!disposed && generation === epoch) {
        clearCapability();
        dirty = false;
        edit(record);
        display("已读取当前服务端配置。", "success");
      }
    } catch (e) {
      if (!disposed && generation === epoch) fail(e);
    } finally {
      requests.delete(controller);
      if (generation === epoch) {
        reading = false;
        editor.inert = false;
        editor.removeAttribute("aria-busy");
        for (const [c, disabled] of controls)
          if (c.isConnected) c.disabled = disabled;
        authorize();
      }
    }
  };
  const showCapability = (value: string) => {
    clearCapability();
    capabilityPending = true;
    const box = node("section");
    box.className = "notice";
    box.append(
      node("h2", "一次性订阅链接"),
      node(
        "p",
        "该链接授予只读访问权限。仅在本页内存中保留，2分钟后或离开/会话失效时清除；GET 无法恢复，遗失请显式轮换。不要放到日志、分析或公开聊天。",
      ),
    );
    const wrap = node("label", "一次性链接（默认隐藏）");
    const secret = node("input");
    secret.type = "password";
    secret.readOnly = true;
    secret.autocomplete = "off";
    wrap.append(secret);
    box.append(wrap);
    const outcome = node("p");
    outcome.setAttribute("role", "status");
    const show = node("button", "显示一次性链接");
    show.type = "button";
    const copy = node("button", "复制一次性链接");
    copy.type = "button";
    const hide = node("button", "已保存，清除本页链接");
    hide.type = "button";
    let capability = value;
    let live = true;
    show.onclick = () => {
      if (!live) return;
      secret.value = capability;
      secret.type = secret.type === "password" ? "text" : "password";
      show.textContent =
        secret.type === "text" ? "隐藏一次性链接" : "显示一次性链接";
    };
    copy.onclick = async () => {
      if (!live) return;
      copy.disabled = true;
      try {
        await navigator.clipboard.writeText(capability);
        if (live)
          outcome.textContent =
            "已复制到系统剪贴板；剪贴板不由本页自动清除，请妥善保管。";
      } catch {
        if (live)
          outcome.textContent =
            "浏览器未允许复制。可显式显示后手动复制；未确认已复制。";
      } finally {
        if (live) copy.disabled = false;
      }
    };
    hide.onclick = () => clearCapability();
    box.append(show, copy, hide, outcome);
    editor.append(box);
    const timer = setTimeout(() => clearCapability(), 120000);
    clearCapability = () => {
      clearTimeout(timer);
      live = false;
      capability = "";
      secret.value = "";
      box.remove();
      capabilityPending = false;
      clearCapability = () => {};
    };
  };
  const edit = (record?: Records[K]) => {
    formAbort?.abort();
    formAbort = new AbortController();
    clearCapability();
    epoch++;
    reading = false;
    editor.inert = false;
    editor.removeAttribute("aria-busy");
    editor.replaceChildren();
    for (const b of [...writes]) if (b !== create) writes.delete(b);
    dirty = false;
    const generation = epoch;
    const isRetired = record ? retired(record) !== null : false;
    let blocked = isRetired;
    const form = node("form");
    form.className = "source-form";
    const fields = node("fieldset");
    const idWrap = node("label", `稳定${noun} ID`);
    const id = node("input");
    id.required = true;
    id.pattern = "[A-Za-z0-9_-]{1,128}";
    id.value = record ? recordId(record) : crypto.randomUUID();
    id.readOnly = !!record;
    idWrap.append(id);
    fields.append(idWrap);
    const read = configurationForm(
      fields,
      api.kind,
      record?.configuration as Configurations[K] | undefined,
      new SourcesAPI(api.session),
      formAbort.signal,
    );
    fields.disabled = isRetired;
    form.append(fields);
    const outcome = node("p");
    outcome.setAttribute("role", "alert");
    form.append(outcome);
    const save = node("button", record ? "保存配置" : `保存禁用${noun}`);
    save.type = "submit";
    const cancel = node("button", "取消 / 返回列表");
    cancel.type = "button";
    cancel.onclick = () => {
      if (canLeave()) {
        epoch++;
        currentWrite?.abort();
        if (writing) api.session.clear();
        formAbort?.abort();
        clearCapability();
        dirty = false;
        editor.replaceChildren();
      }
    };
    const verify = node("button", "读取最新版本（保留输入）");
    verify.type = "button";
    const compare = node("div");
    const cancelWait = node("button", "取消等待并核实结果");
    cancelWait.type = "button";
    cancelWait.hidden = true;
    cancelWait.onclick = () => {
      if (!currentWrite) return;
      currentWrite.abort();
      api.session.clear();
      blocked = true;
      outcome.textContent =
        "写入结果未知：取消等待不等于撤回。请重新登录并读取核实，勿重复提交。一次性链接丢失需核对后显式轮换。";
      display(outcome.textContent);
    };
    const localWrites = new Set<HTMLButtonElement>([save]);
    writes.add(save);
    const mark = () => {
      for (const b of localWrites) b.dataset.blocked = String(blocked);
      authorize();
    };
    const write = async (
      operation: (signal: AbortSignal) => Promise<void>,
      needsValidity = false,
    ) => {
      if (
        writing ||
        reading ||
        blocked ||
        !validId(id.value) ||
        (needsValidity && !form.reportValidity())
      )
        return;
      if (!api.session.writable) {
        api.session.clear();
        display("写入授权已失效，请重新登录。");
        return;
      }
      const controller = new AbortController();
      requests.add(controller);
      currentWrite = controller;
      writing = true;
      fields.disabled = true;
      verify.disabled = true;
      cancelWait.hidden = false;
      clearCapability();
      authorize();
      outcome.textContent = "";
      display("写入中，请等待服务端回执…", "pending");
      let confirmed = false;
      try {
        await operation(controller.signal);
        confirmed = !controller.signal.aborted;
      } catch (e) {
        if (!disposed && generation === epoch && !controller.signal.aborted) {
          if (
            !(e instanceof AdminFailure) ||
            ["unknown", "conflict", "denied"].includes(e.kind)
          )
            blocked = true;
          outcome.textContent =
            e instanceof AdminFailure
              ? e.message
              : "未确认写入结果，请重新读取核实。";
          fail(e);
        }
      } finally {
        requests.delete(controller);
        if (currentWrite === controller) currentWrite = undefined;
        writing = false;
        verify.disabled = false;
        cancelWait.hidden = true;
        fields.disabled = blocked || isRetired;
        mark();
        if (confirmed && !disposed) void load();
      }
    };
    form.oninput = () => {
      dirty = true;
    };
    form.onsubmit = (event) => {
      event.preventDefault();
      if (writing || blocked || !form.reportValidity()) return;
      if (capabilityPending && !canLeave()) return;
      let config: ReturnType<typeof read>;
      try {
        config = read();
      } catch (e) {
        fail(e);
        return;
      }
      void write(async (signal) => {
        const saved = await api.save(
          id.value,
          config,
          record ? revision(record) : null,
          signal,
        );
        if (disposed || signal.aborted || generation !== epoch) return;
        edit(saved.record);
        if (saved.capability) showCapability(saved.capability);
        display(
          `服务端已确认保存配置版本 ${revision(saved.record)}；不代表日历已刷新或通知已显示。`,
          "success",
        );
      }, true);
    };
    verify.onclick = async () => {
      if (writing || !validId(id.value)) return;
      const controller = new AbortController();
      requests.add(controller);
      verify.disabled = true;
      try {
        const latest = await api.read(id.value, controller.signal);
        if (disposed || generation !== epoch) return;
        compare.replaceChildren(
          node(
            "p",
            `当前服务端：${latest.configuration.name} · 版本 ${revision(latest)} · ${retired(latest) === null ? "未退休" : "永久退休"}。原输入保留；GET 不提供链接密钥。`,
          ),
        );
        const adopt = node("button", "采用最新版本重新编辑（放弃原输入）");
        adopt.type = "button";
        adopt.onclick = () => {
          if (!writing && generation === epoch && canLeave()) {
            edit(latest);
            display("已显式采用服务端最新配置。", "success");
          }
        };
        compare.append(adopt);
      } catch (e) {
        if (!disposed && generation === epoch) fail(e);
      } finally {
        requests.delete(controller);
        verify.disabled = false;
      }
    };
    form.append(save, cancel, verify, cancelWait, compare);
    if (record && !isRetired) {
      const retire = node("button", `退休${noun}`);
      retire.type = "button";
      const confirmation = node("div");
      confirmation.hidden = true;
      const confirm = node("button", "确认永久退休");
      confirm.type = "button";
      const back = node("button", "保留配置");
      back.type = "button";
      back.onclick = () => {
        confirmation.hidden = true;
      };
      confirmation.append(
        node(
          "p",
          "永久退休使后续任务或订阅权限失效，保留历史；同一 ID 不可复活，已发出的请求无法原子撤回。未保存输入将放弃。",
        ),
        confirm,
        back,
      );
      retire.onclick = () => {
        if (!writing && !blocked) confirmation.hidden = false;
      };
      confirm.onclick = () => {
        if (capabilityPending && !canLeave()) return;
        void write(async (signal) => {
          await api.retire(recordId(record), revision(record), signal);
          if (!disposed && !signal.aborted && generation === epoch) {
            blocked = true;
            dirty = false;
            fields.disabled = true;
            display("服务端已确认永久退休；同一 ID 不可复活。", "success");
          }
        });
      };
      for (const b of [retire, confirm]) {
        writes.add(b);
        localWrites.add(b);
      }
      form.append(retire, confirmation);
      if (api.kind === "subscriptions") {
        const rotate = node("button", "轮换订阅链接");
        rotate.type = "button";
        writes.add(rotate);
        localWrites.add(rotate);
        rotate.onclick = () => {
          if ((dirty || capabilityPending) && !canLeave()) return;
          if (
            !window.confirm(
              "轮换会立即使旧链接失效；普通配置编辑不会换链接。确认轮换当前已保存版本？",
            )
          )
            return;
          void write(async (signal) => {
            const saved = await api.rotate(
              recordId(record),
              revision(record),
              signal,
            );
            if (disposed || signal.aborted || generation !== epoch) return;
            edit(saved.record);
            showCapability(saved.capability);
            display(
              `服务端已确认轮换至版本 ${revision(saved.record)}；旧链接已失效，请自行更新客户端。`,
              "success",
            );
          });
        };
        form.append(rotate);
      }
    }
    if (isRetired)
      editor.append(
        node("p", "此配置已永久退休，只读保留；不可启用、修改或复活。"),
      );
    editor.append(form);
    mark();
  };
  create.onclick = () => {
    if (!writing && !reading && canLeave()) edit();
  };
  refresh.onclick = () => {
    void load();
  };
  const unload = (event: BeforeUnloadEvent) => {
    if (dirty || writing || capabilityPending) event.preventDefault();
  };
  window.addEventListener("beforeunload", unload);
  authorize();
  void load();
  return {
    canLeave,
    dispose: () => {
      disposed = true;
      epoch++;
      formAbort?.abort();
      clearCapability();
      for (const c of requests) c.abort();
      if (writing) api.session.clear();
      api.session.removeEventListener("change", sessionChanged);
      window.removeEventListener("beforeunload", unload);
    },
  };
}
